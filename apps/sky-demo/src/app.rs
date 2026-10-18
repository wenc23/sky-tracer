use std::error::Error;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{WindowAttributes, WindowId};

use crate::assets::RealtimeAsset;
use crate::catalog::{CatalogScan, scan_asset_root};
use crate::color::DisplayTransform;
use crate::controls::{
    Climate, CloudQuality, EffectsQuality, RealtimeControls, WeatherKind, WeatherModeControl,
};
use crate::experiment::{
    CompareMode, ExperimentInit, FrameContext, RealtimeExperiment, SurfaceViewport, UpdateContext,
};
use crate::gpu::{GpuContext, SurfaceFrameStatus};
use crate::passes::offline_lut::OfflineLutExperiment;
use crate::ui_renderer::UiRenderer;
use crate::workbench::{WorkbenchAction, WorkbenchMode, WorkbenchState, WorkbenchStatus};

/// Which workbench layout/experiment the application shell starts with.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AppMode {
    #[default]
    Reference,
    Weather,
}

/// Workbench startup values for the real-time behaviour-weather workbench.
/// These map one-to-one onto `RealtimeControls` fields; the workbench UI can
/// still change all of them interactively after launch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeatherSession {
    pub mode: WeatherModeControl,
    pub kind: WeatherKind,
    pub climate: Climate,
    pub intensity: f32,
    pub speed: f32,
    pub seed: u64,
    pub auto_sun: bool,
    pub season: f32,
    pub latitude_deg: f32,
    pub day_of_year: f32,
    pub start_hour_utc: f32,
    pub sun_elevation_deg: f32,
    pub sun_azimuth_deg: f32,
    pub observer_altitude_km: f32,
    pub yaw_deg: f32,
    pub pitch_deg: f32,
    pub fov_y_deg: f32,
    pub cloud_quality: CloudQuality,
    pub effects_quality: EffectsQuality,
    pub exposure_ev: f32,
    /// Scales the weather-driven cloud occlusion (0 = none, 1 = automatic).
    pub cloud_occlusion: f32,
}

impl Default for WeatherSession {
    fn default() -> Self {
        Self {
            mode: WeatherModeControl::Auto,
            kind: WeatherKind::PartlyCloudy,
            climate: Climate::Temperate,
            intensity: 1.0,
            speed: 60.0,
            seed: crate::controls::DEFAULT_WEATHER_SEED,
            auto_sun: true,
            season: crate::controls::DEFAULT_WEATHER_SEASON,
            latitude_deg: crate::controls::DEFAULT_WEATHER_LATITUDE_DEG,
            day_of_year: crate::controls::DEFAULT_WEATHER_DAY_OF_YEAR,
            start_hour_utc: crate::controls::DEFAULT_WEATHER_START_HOUR_UTC,
            sun_elevation_deg: 30.0,
            sun_azimuth_deg: 180.0,
            observer_altitude_km: 0.2,
            yaw_deg: 0.0,
            pitch_deg: 18.0,
            fov_y_deg: 70.0,
            cloud_quality: CloudQuality::Balanced,
            effects_quality: EffectsQuality::High,
            exposure_ev: 0.0,
            cloud_occlusion: 1.0,
        }
    }
}

impl WeatherSession {
    pub fn apply(&self, controls: &mut RealtimeControls) {
        controls.weather_enabled = true;
        controls.weather_mode = self.mode;
        controls.weather_kind = self.kind;
        controls.weather_climate = self.climate;
        controls.weather_intensity = self.intensity;
        controls.weather_speed = self.speed;
        controls.weather_seed = self.seed;
        controls.auto_sun = self.auto_sun;
        controls.weather_season = self.season;
        controls.weather_latitude_deg = self.latitude_deg;
        controls.weather_day_of_year = self.day_of_year;
        controls.weather_start_hour_utc = self.start_hour_utc;
        controls.sun_elevation_deg = self.sun_elevation_deg;
        controls.sun_azimuth_deg = self.sun_azimuth_deg;
        controls.observer_altitude_km = self.observer_altitude_km;
        controls.view.yaw_deg = self.yaw_deg;
        controls.view.pitch_deg = self.pitch_deg;
        controls.view.fov_y_deg = self.fov_y_deg;
        controls.cloud_quality = self.cloud_quality;
        controls.weather_effects = self.effects_quality;
        controls.cloud_occlusion = self.cloud_occlusion;
        controls.exposure_ev = self.exposure_ev;
        *controls = controls.normalized();
    }
}

/// Offscreen capture configuration: render real frames through the same
/// experiment + egui pipeline, then write a PNG and exit. Used for automated
/// verification of the real-time workbench (no human input required).
#[derive(Clone, Debug)]
pub struct CaptureOptions {
    pub frames: u32,
    pub screenshot: PathBuf,
    pub width: u32,
    pub height: u32,
}

pub struct RunConfig {
    pub asset_path: Option<PathBuf>,
    pub experiment: ExperimentKind,
    pub lut_path: PathBuf,
    pub mode: AppMode,
    pub weather: Option<WeatherSession>,
    pub capture: Option<CaptureOptions>,
    /// Simulated seconds of weather spin-up before the first presented frame.
    pub warmup_seconds: f64,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum ExperimentKind {
    #[value(name = "realtime", alias = "hybrid-4d")]
    Hybrid4d,
    #[value(name = "reference", alias = "offline-lut")]
    OfflineLut,
    #[value(name = "weather", alias = "clouds")]
    Weather,
}

/// Render target for a frame: the window surface or the offscreen capture
/// texture used by `--smoke`.
enum FrameTarget {
    Surface {
        texture: wgpu::SurfaceTexture,
        view: wgpu::TextureView,
        reconfigure_after_present: bool,
    },
    Capture {
        view: wgpu::TextureView,
    },
}

/// Offscreen PNG capture state for `--smoke`. Frames are produced by the
/// normal experiment + egui path (only the surface acquire/present is skipped),
/// so the captured image is evidence that the workbench actually rendered.
struct CaptureState {
    options: CaptureOptions,
    texture: Option<wgpu::Texture>,
    size: PhysicalSize<u32>,
    frame_index: u32,
    started: Instant,
    frame_ms: Vec<f32>,
}

impl CaptureState {
    fn new(options: CaptureOptions) -> Self {
        let now = Instant::now();
        Self {
            options,
            texture: None,
            size: PhysicalSize::new(0, 0),
            frame_index: 0,
            started: now,
            frame_ms: Vec::new(),
        }
    }

    fn ensure_target(
        &mut self,
        device: &wgpu::Device,
        size: PhysicalSize<u32>,
        format: wgpu::TextureFormat,
    ) {
        let width = size.width.max(1);
        let height = size.height.max(1);
        if self.size.width == width && self.size.height == height && self.texture.is_some() {
            return;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sky_weather_workbench_capture_target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        self.texture = Some(texture);
        self.size = PhysicalSize::new(width, height);
    }
}

#[derive(Debug)]
enum UserEvent {
    RepaintAfter(Duration),
    CatalogScanned {
        generation: u64,
        result: Result<CatalogScan, String>,
    },
}

pub fn run(config: RunConfig) -> Result<(), Box<dyn Error>> {
    if let Some(capture) = &config.capture {
        if capture.frames == 0 {
            return Err("capture frame count must be at least 1".into());
        }
        if capture.width == 0 || capture.height == 0 {
            return Err("capture width and height must be non-zero".into());
        }
        if capture.screenshot.as_os_str().is_empty() {
            return Err("capture requires a screenshot path".into());
        }
    }
    if !config.warmup_seconds.is_finite() || config.warmup_seconds < 0.0 {
        return Err("warm-up seconds must be finite and non-negative".into());
    }
    let asset = match &config.asset_path {
        Some(path) => RealtimeAsset::load(path)?,
        None => RealtimeAsset::demo_default(),
    };
    println!("loaded asset: {}", asset.summary_line());
    warn_missing_files(&asset);

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let workbench_mode = match config.mode {
        AppMode::Reference => WorkbenchMode::Reference,
        AppMode::Weather => WorkbenchMode::Weather,
    };
    let mut workbench = WorkbenchState::new(&asset, workbench_mode);
    if let Some(session) = &config.weather {
        session.apply(&mut workbench.controls);
    }
    workbench.hybrid_atmosphere =
        matches!(config.experiment, ExperimentKind::Hybrid4d) && config.mode == AppMode::Reference;
    workbench.weather_controls_visible =
        config.mode == AppMode::Weather || matches!(config.experiment, ExperimentKind::Weather);
    if matches!(config.experiment, ExperimentKind::OfflineLut) {
        let lut = sky_reference::asset::Manifest::open(&config.lut_path)
            .map_err(|e| io::Error::other(e.to_string()))?;
        workbench.baked_atmosphere = Some([
            lut.geometry.bottom,
            lut.geometry.top_height(),
            lut.config.ground_albedo,
        ]);
    }
    let warmup_seconds = config.warmup_seconds;
    let capture = config.capture.map(CaptureState::new);
    let mut app = DemoApp {
        asset,
        gpu: None,
        experiment: None,
        experiment_kind: config.experiment,
        lut_path: config.lut_path,
        ui: None,
        workbench,
        proxy,
        modifiers: ModifiersState::default(),
        cursor_position: None,
        last_viewport: SurfaceViewport {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        pending_actions: Vec::new(),
        next_repaint: None,
        last_status_title: Instant::now(),
        init_error: None,
        mode: config.mode,
        capture,
        capture_done: false,
        warmup_seconds,
        fps: 0.0,
        last_frame_instant: Instant::now(),
    };
    event_loop.run_app(&mut app)?;
    if let Some(error) = app.init_error {
        return Err(io::Error::other(error).into());
    }
    Ok(())
}

struct DemoApp {
    asset: RealtimeAsset,
    gpu: Option<GpuContext>,
    experiment: Option<Box<dyn RealtimeExperiment>>,
    experiment_kind: ExperimentKind,
    lut_path: PathBuf,
    ui: Option<UiRenderer>,
    workbench: WorkbenchState,
    proxy: EventLoopProxy<UserEvent>,
    modifiers: ModifiersState,
    cursor_position: Option<PhysicalPosition<f64>>,
    last_viewport: SurfaceViewport,
    pending_actions: Vec<WorkbenchAction>,
    next_repaint: Option<Instant>,
    last_status_title: Instant,
    init_error: Option<String>,
    mode: AppMode,
    capture: Option<CaptureState>,
    capture_done: bool,
    warmup_seconds: f64,
    fps: f32,
    last_frame_instant: Instant,
}

impl DemoApp {
    fn create_experiment(
        kind: ExperimentKind,
        gpu: &GpuContext,
        asset: &RealtimeAsset,
        lut_path: &std::path::Path,
    ) -> Result<Box<dyn RealtimeExperiment>, String> {
        let init = ExperimentInit {
            device: gpu.device(),
            queue: gpu.queue(),
            surface_format: gpu.surface_format(),
            asset,
            display: DisplayTransform::default(),
        };
        match kind {
            ExperimentKind::Hybrid4d => crate::passes::hybrid::HybridExperiment::new(init)
                .map(|experiment| Box::new(experiment) as Box<dyn RealtimeExperiment>),
            ExperimentKind::OfflineLut => OfflineLutExperiment::new(init, lut_path)
                .map(|experiment| Box::new(experiment) as Box<dyn RealtimeExperiment>),
            ExperimentKind::Weather => crate::passes::weather::WeatherExperiment::new(init)
                .map(|experiment| Box::new(experiment) as Box<dyn RealtimeExperiment>),
        }
    }

    fn window_id(&self) -> Option<WindowId> {
        self.gpu.as_ref().map(|gpu| gpu.window().id())
    }

    fn window_title(&self) -> String {
        match self.mode {
            AppMode::Weather => "天气可视化工作台".to_owned(),
            AppMode::Reference => self.asset.title(),
        }
    }

    fn request_redraw(&self) {
        if let Some(gpu) = &self.gpu {
            gpu.window().request_redraw();
        }
    }

    fn create_ui_renderer(&self, gpu: &GpuContext) -> UiRenderer {
        let proxy = self.proxy.clone();
        UiRenderer::new(
            gpu.window(),
            gpu.device(),
            gpu.surface_format(),
            move |delay| {
                let _ = proxy.send_event(UserEvent::RepaintAfter(delay));
            },
        )
    }

    /// Apply startup controls (and optional behaviour warm-up) to a freshly
    /// created experiment. Must run before the first presented frame so the
    /// weather workbench opens on the configured state instead of rebuilding
    /// the behaviour model on frame one.
    fn initialize_experiment(
        &self,
        experiment: &mut Box<dyn RealtimeExperiment>,
    ) -> Result<(), String> {
        experiment.configure(&self.workbench.controls)?;
        if self.warmup_seconds > 0.0 {
            experiment.warm_up(self.warmup_seconds)?;
        }
        Ok(())
    }

    fn synchronize_display_mode(&mut self) -> bool {
        let desired_hdr = self.workbench.controls.hdr_enabled;
        let Some(gpu) = &self.gpu else {
            return false;
        };
        if desired_hdr == gpu.hdr_enabled() {
            return false;
        }
        if desired_hdr && !gpu.hdr_supported() {
            self.workbench.controls.hdr_enabled = false;
            self.workbench
                .set_error("HDR unavailable: this surface has no scRGB Rgba16Float format.");
            self.request_redraw();
            return false;
        }

        let previous_hdr = gpu.hdr_enabled();
        if let Err(error) = self
            .gpu
            .as_mut()
            .expect("GPU checked above")
            .set_hdr_enabled(desired_hdr)
        {
            self.workbench.controls.hdr_enabled = previous_hdr;
            self.workbench
                .set_error(format!("HDR switch failed: {error}"));
            self.request_redraw();
            return false;
        }

        let next_experiment = {
            let gpu = self.gpu.as_ref().expect("GPU checked above");
            Self::create_experiment(self.experiment_kind, gpu, &self.asset, &self.lut_path)
        };
        let next_experiment = match next_experiment {
            Ok(experiment) => experiment,
            Err(error) => {
                let _ = self
                    .gpu
                    .as_mut()
                    .expect("GPU checked above")
                    .set_hdr_enabled(previous_hdr);
                self.workbench.controls.hdr_enabled = previous_hdr;
                self.workbench
                    .set_error(format!("HDR pipeline rebuild failed: {error}"));
                self.request_redraw();
                return false;
            }
        };
        let mut next_experiment = next_experiment;
        if let Err(error) = self.initialize_experiment(&mut next_experiment) {
            let _ = self
                .gpu
                .as_mut()
                .expect("GPU checked above")
                .set_hdr_enabled(previous_hdr);
            self.workbench.controls.hdr_enabled = previous_hdr;
            self.workbench
                .set_error(format!("HDR pipeline rebuild failed: {error}"));
            self.request_redraw();
            return false;
        }

        let next_ui = {
            let gpu = self.gpu.as_ref().expect("GPU checked above");
            self.create_ui_renderer(gpu)
        };
        self.experiment = Some(next_experiment);
        self.ui = Some(next_ui);
        if let Some(gpu) = &self.gpu {
            gpu.window().set_title(&format!(
                "{}{}",
                self.window_title(),
                if desired_hdr { " [HDR scRGB]" } else { "" }
            ));
        }
        true
    }

    fn start_catalog_scan(&mut self) {
        let (generation, root) = self.workbench.begin_scan();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = scan_asset_root(root);
            let _ = proxy.send_event(UserEvent::CatalogScanned { generation, result });
        });
    }

    fn load_asset(&mut self, path: PathBuf) {
        let Some(gpu) = &self.gpu else {
            self.workbench.set_error("GPU is not initialized.");
            return;
        };
        let candidate = prepare_asset_change(path.clone(), |asset| {
            Self::create_experiment(self.experiment_kind, gpu, asset, &self.lut_path)
        });
        let (next_asset, mut next_experiment) = match candidate {
            Ok(candidate) => candidate,
            Err(error) => {
                self.workbench.set_error(error);
                return;
            }
        };
        if let Err(error) = self.initialize_experiment(&mut next_experiment) {
            self.workbench.set_error(error);
            return;
        }

        warn_missing_files(&next_asset);
        self.asset = next_asset;
        self.experiment = Some(next_experiment);
        self.workbench.set_current_asset(&self.asset);
        if !self.reference_available()
            && self.workbench.controls.compare_mode != CompareMode::Realtime
        {
            self.workbench.controls.compare_mode = CompareMode::Realtime;
        }
        if let Some(gpu) = &self.gpu {
            gpu.window().set_title(&format!(
                "{}{}",
                self.window_title(),
                if gpu.hdr_enabled() {
                    " [HDR scRGB]"
                } else {
                    ""
                }
            ));
        }
        println!("loaded asset: {}", self.asset.summary_line());
    }

    fn reference_available(&self) -> bool {
        self.experiment
            .as_ref()
            .is_some_and(|experiment| experiment.reference_available())
    }

    fn process_actions(&mut self, actions: impl IntoIterator<Item = WorkbenchAction>) {
        for action in actions {
            match action {
                WorkbenchAction::OpenAsset => {
                    let mut dialog = rfd::FileDialog::new()
                        .add_filter("Sky asset manifest", &["json"])
                        .set_title("Open Sky Asset");
                    if self.workbench.asset_root().is_dir() {
                        dialog = dialog.set_directory(self.workbench.asset_root());
                    }
                    if let Some(path) = dialog.pick_file() {
                        self.load_asset(path);
                    }
                }
                WorkbenchAction::ChooseAssetRoot => {
                    let mut dialog = rfd::FileDialog::new().set_title("Choose Asset Root");
                    if self.workbench.asset_root().is_dir() {
                        dialog = dialog.set_directory(self.workbench.asset_root());
                    }
                    if let Some(root) = dialog.pick_folder() {
                        self.workbench.set_asset_root(root);
                        self.start_catalog_scan();
                    }
                }
                WorkbenchAction::RefreshCatalog => self.start_catalog_scan(),
                WorkbenchAction::LoadAsset(path) => self.load_asset(path),
            }
        }
    }

    fn render_frame(&mut self, event_loop: &ActiveEventLoop) {
        if self.capture_done {
            return;
        }
        let reference_available = self.reference_available();
        let (hdr_supported, hdr_active) = self.gpu.as_ref().map_or((false, false), |gpu| {
            (gpu.hdr_supported(), gpu.hdr_enabled())
        });
        let weather_hud = self
            .experiment
            .as_ref()
            .and_then(|experiment| experiment.weather_hud());
        let experiment_name = self
            .experiment
            .as_ref()
            .map_or("", |experiment| experiment.name());
        let status = WorkbenchStatus {
            fps: self.fps,
            experiment_name,
            weather: weather_hud,
            capture: self.capture.is_some(),
        };
        let (workbench_frame, prepared_ui) = {
            let Some(gpu) = &self.gpu else {
                return;
            };
            let Some(ui) = &mut self.ui else {
                return;
            };
            ui.run(gpu.window(), |root| {
                self.workbench.show(
                    root,
                    &self.asset,
                    reference_available,
                    hdr_supported,
                    hdr_active,
                    &status,
                )
            })
        };

        let mut actions = std::mem::take(&mut self.pending_actions);
        actions.extend(workbench_frame.actions);
        self.process_actions(actions);
        if self.synchronize_display_mode() {
            self.request_redraw();
            return;
        }

        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let viewport = physical_viewport(
            workbench_frame.viewport_points,
            prepared_ui.pixels_per_point(),
            gpu.size(),
        );
        self.last_viewport = viewport;
        let controls = self.workbench.controls;

        let target = match self.capture.as_ref() {
            Some(capture) => {
                let texture = capture
                    .texture
                    .as_ref()
                    .expect("capture target initialized in resumed()");
                FrameTarget::Capture {
                    view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
                }
            }
            None => match gpu.acquire_frame() {
                Ok(frame) => FrameTarget::Surface {
                    view: frame
                        .texture
                        .texture
                        .create_view(&wgpu::TextureViewDescriptor::default()),
                    texture: frame.texture,
                    reconfigure_after_present: frame.reconfigure_after_present,
                },
                Err(SurfaceFrameStatus::Reconfigure) => {
                    if let Some(gpu) = self.gpu.as_mut() {
                        gpu.resize(gpu.size());
                    }
                    self.request_redraw();
                    return;
                }
                Err(SurfaceFrameStatus::Skip) => return,
                Err(SurfaceFrameStatus::Exit) => {
                    event_loop.exit();
                    return;
                }
            },
        };
        let target_view = match &target {
            FrameTarget::Surface { view, .. } => view,
            FrameTarget::Capture { view } => view,
        };

        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sky_realtime_demo_frame_encoder"),
            });

        if matches!(target, FrameTarget::Capture { .. }) {
            // The capture target is reused between frames and the egui pass
            // loads instead of clearing, so start from an explicit black frame.
            let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("weather_workbench_capture_clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        let user_command_buffers = self
            .ui
            .as_mut()
            .expect("UI initialized with GPU")
            .prepare_gpu(gpu.device(), gpu.queue(), &mut encoder, &prepared_ui);

        let experiment = self
            .experiment
            .as_mut()
            .expect("experiment initialized with GPU");
        experiment.update(UpdateContext {
            controls: &controls,
        });
        experiment.render(FrameContext {
            device: gpu.device(),
            queue: gpu.queue(),
            encoder: &mut encoder,
            target: target_view,
            viewport,
        });
        self.ui.as_mut().expect("UI initialized with GPU").render(
            &mut encoder,
            target_view,
            &prepared_ui,
        );

        gpu.window().pre_present_notify();
        gpu.queue()
            .submit(user_command_buffers.into_iter().chain([encoder.finish()]));

        let capture_active = matches!(target, FrameTarget::Capture { .. });
        if capture_active {
            // Include GPU completion in the measured frame time so the
            // reported capture throughput is not submission-only.
            let _ = gpu.device().poll(wgpu::PollType::wait_indefinitely());
        }
        let now = Instant::now();
        let frame_seconds = now.duration_since(self.last_frame_instant).as_secs_f32();
        self.last_frame_instant = now;
        if frame_seconds > 1.0e-6 {
            let instantaneous = 1.0 / frame_seconds;
            self.fps = if self.fps <= 1.0e-3 {
                instantaneous
            } else {
                self.fps * 0.9 + instantaneous * 0.1
            };
        }

        let mut capture_finished = false;
        match target {
            FrameTarget::Surface {
                texture,
                reconfigure_after_present,
                ..
            } => {
                texture.present();
                if reconfigure_after_present {
                    if let Some(gpu) = self.gpu.as_mut() {
                        gpu.resize(gpu.size());
                    }
                    self.request_redraw();
                }
            }
            FrameTarget::Capture { .. } => {
                let capture = self.capture.as_mut().expect("capture active");
                capture.frame_index = capture.frame_index.saturating_add(1);
                capture.frame_ms.push(frame_seconds * 1000.0);
                capture_finished = capture.frame_index >= capture.options.frames;
            }
        }

        self.ui
            .as_mut()
            .expect("UI initialized with GPU")
            .free_textures(&prepared_ui);

        if capture_finished {
            self.finish_capture(event_loop);
            return;
        }

        if self.last_status_title.elapsed() >= Duration::from_secs(1) {
            let status = self
                .experiment
                .as_ref()
                .and_then(|experiment| experiment.status_line());
            if let (Some(gpu), Some(status)) = (&self.gpu, status) {
                let hdr = if gpu.hdr_enabled() {
                    " [HDR scRGB]"
                } else {
                    ""
                };
                gpu.window()
                    .set_title(&format!("{}{hdr} | {status}", self.window_title()));
            }
            self.last_status_title = Instant::now();
        }
    }

    /// Read the final offscreen frame back and write it as a PNG, then exit.
    fn finish_capture(&mut self, event_loop: &ActiveEventLoop) {
        if self.capture_done {
            return;
        }
        self.capture_done = true;
        let Some(gpu) = self.gpu.as_ref() else {
            event_loop.exit();
            return;
        };
        let Some(capture) = self.capture.as_ref() else {
            event_loop.exit();
            return;
        };
        let Some(texture) = capture.texture.as_ref() else {
            self.init_error = Some("截图渲染目标尚未创建".to_owned());
            event_loop.exit();
            return;
        };
        let width = texture.width();
        let height = texture.height();
        let format = gpu.surface_format();
        let screenshot = capture.options.screenshot.clone();
        let frame_ms = capture.frame_ms.clone();
        let elapsed = capture.started.elapsed().as_secs_f32();
        let readback = sky_realtime::read_texture(gpu.device(), gpu.queue(), texture, 4);

        let bytes = match readback {
            Ok(bytes) => bytes,
            Err(error) => {
                self.init_error = Some(format!("截图回读失败：{error}"));
                event_loop.exit();
                return;
            }
        };
        let mut rgba = Vec::with_capacity(bytes.len());
        match format {
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => {
                for pixel in bytes.chunks_exact(4) {
                    rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
                }
            }
            _ => {
                for pixel in bytes.chunks_exact(4) {
                    rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
                }
            }
        }
        if let Some(parent) = screenshot
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) =
            image::save_buffer(&screenshot, &rgba, width, height, image::ColorType::Rgba8)
        {
            self.init_error = Some(format!("写入截图 {} 失败：{error}", screenshot.display()));
            event_loop.exit();
            return;
        }
        // The first captured frame includes lazy pipeline creation, the first
        // cloud-volume upload and the first SkyView rebuild; interactive
        // steady-state throughput is reported from the remaining frames.
        let warm_frames = if frame_ms.len() > 2 { 1 } else { 0 };
        let steady = &frame_ms[warm_frames..];
        let mut sorted = steady.to_vec();
        sorted.sort_by(f32::total_cmp);
        let average_ms = if sorted.is_empty() {
            0.0
        } else {
            sorted.iter().sum::<f32>() / sorted.len() as f32
        };
        let median_ms = sorted.get(sorted.len() / 2).copied().unwrap_or(average_ms);
        let p95_ms = sorted
            .get((sorted.len() * 95 / 100).min(sorted.len().saturating_sub(1)))
            .copied()
            .unwrap_or(average_ms);
        let max_ms = sorted.last().copied().unwrap_or(average_ms);
        let min_ms = sorted.first().copied().unwrap_or(average_ms);
        println!(
            "天气工作台截图：{}x{} 格式 {:?}，{} 帧共 {:.2} s -> {}\n\
             稳态（{} 帧）：中位 {:.2} ms（{:.1} fps），均值 {:.2} ms，\
             最小 {:.2}，P95 {:.2}，最大 {:.2} ms",
            width,
            height,
            format,
            frame_ms.len(),
            elapsed,
            screenshot.display(),
            steady.len(),
            median_ms,
            if median_ms > 0.0 {
                1000.0 / median_ms
            } else {
                0.0
            },
            average_ms,
            min_ms,
            p95_ms,
            max_ms,
        );
        event_loop.exit();
    }

    fn handle_shortcut(&mut self, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed || event.repeat {
            return false;
        }
        let PhysicalKey::Code(key) = event.physical_key else {
            return false;
        };
        if !self.cursor_inside_viewport() {
            return false;
        }

        if self.modifiers.control_key() && key == KeyCode::KeyO {
            self.pending_actions.push(WorkbenchAction::OpenAsset);
            return true;
        }
        match key {
            KeyCode::F5 => {
                self.pending_actions.push(WorkbenchAction::RefreshCatalog);
                return true;
            }
            KeyCode::PageUp => {
                if let Some(path) = self.workbench.step_asset(-1) {
                    self.pending_actions.push(WorkbenchAction::LoadAsset(path));
                }
                return true;
            }
            KeyCode::PageDown => {
                if let Some(path) = self.workbench.step_asset(1) {
                    self.pending_actions.push(WorkbenchAction::LoadAsset(path));
                }
                return true;
            }
            KeyCode::F6 if self.gpu.as_ref().is_some_and(GpuContext::hdr_supported) => {
                self.workbench.controls.hdr_enabled = !self.workbench.controls.hdr_enabled;
                return true;
            }
            _ => {}
        }

        if self.mode == AppMode::Weather {
            match key {
                KeyCode::Space => {
                    self.workbench.controls.weather_paused =
                        !self.workbench.controls.weather_paused;
                }
                KeyCode::BracketLeft => {
                    self.workbench.controls.auto_sun = false;
                    self.workbench.controls.sun_elevation_deg -= 1.0;
                }
                KeyCode::BracketRight => {
                    self.workbench.controls.auto_sun = false;
                    self.workbench.controls.sun_elevation_deg += 1.0;
                }
                KeyCode::Comma => {
                    self.workbench.controls.auto_sun = false;
                    self.workbench.controls.sun_azimuth_deg -= 10.0;
                }
                KeyCode::Period => {
                    self.workbench.controls.auto_sun = false;
                    self.workbench.controls.sun_azimuth_deg += 10.0;
                }
                KeyCode::KeyR => self.workbench.controls.reset_view(),
                _ => return false,
            }
            self.workbench.controls = self.workbench.controls.normalized();
            return true;
        }

        let reference_available = self.reference_available();
        match key {
            KeyCode::Digit1 => self.workbench.controls.compare_mode = CompareMode::Realtime,
            KeyCode::Digit2 if reference_available => {
                self.workbench.controls.compare_mode = CompareMode::Reference;
            }
            KeyCode::Digit3 if reference_available => {
                self.workbench.controls.compare_mode = CompareMode::AbsoluteDifference;
            }
            KeyCode::Digit4 if reference_available => {
                self.workbench.controls.compare_mode = CompareMode::SignedDifference;
            }
            KeyCode::KeyD if reference_available => {
                self.workbench.controls.compare_mode = self.workbench.controls.compare_mode.next();
            }
            KeyCode::BracketLeft => self.workbench.controls.sun_elevation_deg -= 1.0,
            KeyCode::BracketRight => self.workbench.controls.sun_elevation_deg += 1.0,
            KeyCode::KeyR => self.workbench.controls.reset_view(),
            _ => return false,
        }
        self.workbench.controls = self.workbench.controls.normalized();
        true
    }

    fn cursor_inside_viewport(&self) -> bool {
        let Some(position) = self.cursor_position else {
            return false;
        };
        let viewport = self.last_viewport;
        position.x >= viewport.x as f64
            && position.y >= viewport.y as f64
            && position.x < (viewport.x + viewport.width) as f64
            && position.y < (viewport.y + viewport.height) as f64
    }
}

fn prepare_asset_change<T>(
    path: PathBuf,
    initialize: impl FnOnce(&RealtimeAsset) -> Result<T, String>,
) -> Result<(RealtimeAsset, T), String> {
    let asset = RealtimeAsset::load(&path)
        .map_err(|error| format!("Failed to load {}: {error}", path.display()))?;
    let prepared = initialize(&asset)
        .map_err(|error| format!("Failed to initialize {}: {error}", path.display()))?;
    Ok((asset, prepared))
}

impl ApplicationHandler<UserEvent> for DemoApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }

        let initial_size: winit::dpi::Size = match &self.capture {
            Some(capture) => {
                PhysicalSize::new(capture.options.width, capture.options.height).into()
            }
            None => event_loop
                .primary_monitor()
                .map_or(LogicalSize::new(1440.0, 900.0), |monitor| {
                    let scale = monitor.scale_factor();
                    let size = monitor.size();
                    LogicalSize::new(
                        1440.0_f64.min(size.width as f64 / scale * 0.9),
                        900.0_f64.min(size.height as f64 / scale * 0.9),
                    )
                })
                .into(),
        };
        let min_size: winit::dpi::Size = if self.capture.is_some() {
            LogicalSize::new(320.0, 200.0).into()
        } else {
            LogicalSize::new(1000.0, 640.0).into()
        };
        let window = match event_loop.create_window(
            WindowAttributes::default()
                .with_title(self.window_title())
                .with_inner_size(initial_size)
                .with_visible(self.capture.is_none())
                .with_min_inner_size(min_size),
        ) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.init_error = Some(error.to_string());
                event_loop.exit();
                return;
            }
        };

        let required_features = match self.experiment_kind {
            ExperimentKind::Hybrid4d => wgpu::Features::empty(),
            ExperimentKind::OfflineLut => wgpu::Features::empty(),
            ExperimentKind::Weather => wgpu::Features::empty(),
        };
        let mut required_limits = wgpu::Limits::default();
        if matches!(self.experiment_kind, ExperimentKind::Hybrid4d) {
            required_limits.max_storage_buffer_binding_size = 512 * 1024 * 1024;
            required_limits.max_buffer_size = 512 * 1024 * 1024;
        }
        if matches!(self.experiment_kind, ExperimentKind::OfflineLut) {
            match sky_reference::asset::Manifest::open(&self.lut_path) {
                Ok(lut) => {
                    let (bytes, total) = sky_reference::renderer::storage_budget(&lut);
                    println!(
                        "offline LUT resident payload: {:.1} MiB",
                        total as f32 / 1048576.0
                    );
                    required_limits.max_storage_buffer_binding_size =
                        required_limits.max_storage_buffer_binding_size.max(bytes);
                    required_limits.max_buffer_size = required_limits.max_buffer_size.max(bytes);
                }
                Err(error) => {
                    self.init_error = Some(error.to_string());
                    event_loop.exit();
                    return;
                }
            }
        }
        let display = DisplayTransform::default();
        println!("display transform: {}", display.output_space.label());
        let gpu = match pollster::block_on(GpuContext::new(
            window.clone(),
            required_features,
            required_limits,
        )) {
            Ok(gpu) => gpu,
            Err(error) => {
                self.init_error = Some(error);
                event_loop.exit();
                return;
            }
        };
        let experiment = match Self::create_experiment(
            self.experiment_kind,
            &gpu,
            &self.asset,
            &self.lut_path,
        ) {
            Ok(experiment) => experiment,
            Err(error) => {
                self.init_error = Some(error);
                event_loop.exit();
                return;
            }
        };

        let ui = self.create_ui_renderer(&gpu);
        println!("当前实时实验：{}", experiment.name());
        if self.mode == AppMode::Weather {
            println!("天气工作台：行为天气 + 体积云 + 屏幕空间天气效果");
            println!("天气快捷键：空格 暂停/继续，[ ] 太阳高度，, . 太阳方位，R 重置视角");
            println!("全部配置都在右侧「设置面板」中，可直接拖动/选择");
        } else {
            println!("view controls: viewport drag = yaw/pitch, viewport wheel = fov, R = reset");
            println!("asset controls: Ctrl+O open, F5 rescan, Page Up/Down navigate");
        }
        println!("display controls: F6 toggles HDR scRGB when supported");

        let mut experiment = experiment;
        if let Err(error) = self.initialize_experiment(&mut experiment) {
            self.init_error = Some(format!("实验预热失败：{error}"));
            event_loop.exit();
            return;
        }
        if self.capture.is_some() {
            // The capture path reads the frame back as RGBA8; force SDR output.
            self.workbench.controls.hdr_enabled = false;
        }

        self.experiment = Some(experiment);
        self.ui = Some(ui);
        self.gpu = Some(gpu);
        if let Some(capture) = self.capture.as_mut() {
            let gpu = self.gpu.as_ref().expect("GPU initialized above");
            capture.ensure_target(gpu.device(), gpu.size(), gpu.surface_format());
            println!(
                "截图模式：{} 帧，{}x{}，输出 -> {}",
                capture.options.frames,
                capture.options.width,
                capture.options.height,
                capture.options.screenshot.display()
            );
        }
        if !self.asset.is_default_view() {
            self.start_catalog_scan();
        }
        self.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if Some(window_id) != self.window_id() {
            return;
        }

        let egui_response = if let (Some(gpu), Some(ui)) = (&self.gpu, &mut self.ui) {
            ui.on_window_event(gpu.window(), &event)
        } else {
            egui_winit::EventResponse::default()
        };
        if egui_response.repaint {
            self.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size);
                }
                if let (Some(capture), Some(gpu)) = (self.capture.as_mut(), self.gpu.as_ref()) {
                    capture.ensure_target(gpu.device(), gpu.size(), gpu.surface_format());
                }
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = Some(position);
                self.request_redraw();
            }
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } => {
                self.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if !egui_response.consumed && self.handle_shortcut(&event) {
                    self.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.render_frame(event_loop),
            _ => {}
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::RepaintAfter(delay) if delay.is_zero() => self.request_redraw(),
            UserEvent::RepaintAfter(delay) => {
                if let Some(deadline) = Instant::now().checked_add(delay) {
                    self.next_repaint = Some(
                        self.next_repaint
                            .map_or(deadline, |current| current.min(deadline)),
                    );
                }
            }
            UserEvent::CatalogScanned { generation, result } => {
                self.workbench.apply_scan(generation, result);
                self.request_redraw();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Offscreen capture is a benchmark loop: drive frames directly instead
        // of waiting for RedrawRequested (a hidden Windows window does not
        // receive WM_PAINT, so request_redraw alone would stall).
        if self.capture.is_some() {
            self.render_frame(event_loop);
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        let weather_running = matches!(self.mode, AppMode::Weather)
            || matches!(self.experiment_kind, ExperimentKind::Weather);
        if weather_running && !self.workbench.controls.weather_paused {
            self.next_repaint = Some(self.next_repaint.map_or_else(
                || Instant::now() + Duration::from_millis(16),
                |deadline| deadline.min(Instant::now() + Duration::from_millis(16)),
            ));
        }
        if let Some(deadline) = self.next_repaint {
            if deadline <= Instant::now() {
                self.next_repaint = None;
                self.request_redraw();
                event_loop.set_control_flow(ControlFlow::Wait);
            } else {
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            }
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if self.init_error.is_none() {
            println!("已关闭实时实验：{}", self.asset.manifest_path().display());
        }
    }
}

fn physical_viewport(
    rect: egui::Rect,
    pixels_per_point: f32,
    surface_size: PhysicalSize<u32>,
) -> SurfaceViewport {
    let surface_width = surface_size.width.max(1);
    let surface_height = surface_size.height.max(1);
    let x0 = (rect.min.x * pixels_per_point)
        .floor()
        .clamp(0.0, (surface_width - 1) as f32) as u32;
    let y0 = (rect.min.y * pixels_per_point)
        .floor()
        .clamp(0.0, (surface_height - 1) as f32) as u32;
    let x1 = (rect.max.x * pixels_per_point)
        .ceil()
        .clamp((x0 + 1) as f32, surface_width as f32) as u32;
    let y1 = (rect.max.y * pixels_per_point)
        .ceil()
        .clamp((y0 + 1) as f32, surface_height as f32) as u32;
    SurfaceViewport {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

fn warn_missing_files(asset: &RealtimeAsset) {
    for missing_file in asset.missing_referenced_files() {
        eprintln!(
            "warning: referenced asset file does not exist: {}",
            missing_file.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use egui::{Rect, pos2};
    use sky_assets::asset::{SpectralAssetFiles, SpectralAssetManifest};
    use winit::dpi::PhysicalSize;

    use super::{physical_viewport, prepare_asset_change};

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

    fn write_manifest(path: &Path) {
        let manifest = SpectralAssetManifest::spectral_panorama(
            [4, 2],
            8,
            1,
            12.0,
            34.0,
            0.2,
            vec![500.0],
            SpectralAssetFiles {
                rgb_exr: "sky.exr".to_owned(),
                rgb_png: "sky.png".to_owned(),
                band_exrs: vec!["band.exr".to_owned()],
            },
        );
        std::fs::write(
            path,
            serde_json::to_vec(&manifest).expect("serialize manifest"),
        )
        .expect("write manifest");
    }

    #[test]
    fn viewport_points_are_clamped_and_scaled_to_surface_pixels() {
        let viewport = physical_viewport(
            Rect::from_min_max(pos2(100.25, 40.0), pos2(700.0, 440.25)),
            1.5,
            PhysicalSize::new(1200, 800),
        );
        assert_eq!(viewport.x, 150);
        assert_eq!(viewport.y, 60);
        assert_eq!(viewport.width, 900);
        assert_eq!(viewport.height, 601);
    }

    #[test]
    fn failed_experiment_initialization_produces_no_load_candidate() {
        let root = std::env::temp_dir().join(format!(
            "sky-realtime-transaction-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create test root");
        let path = root.join("asset.json");
        write_manifest(&path);

        let result: Result<(_, ()), _> =
            prepare_asset_change(path, |_| Err("synthetic GPU failure".to_owned()));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("synthetic GPU failure"));

        std::fs::remove_dir_all(root).expect("remove test root");
    }
}
