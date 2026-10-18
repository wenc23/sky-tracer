use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use crate::{
    controls::{
        CloudQuality, EffectsQuality, RealtimeControls, WeatherKind, WeatherModeControl,
        precipitation_kind_label, weather_kind_label,
    },
    experiment::{ExperimentInit, FrameContext, RealtimeExperiment, UpdateContext, WeatherHud},
    passes::common::{ReferenceTexture, TexturePresentPass},
};
use sky_realtime::{Config, Renderer, View, Wavelengths};

const ENVIRONMENT_SIZE: [u32; 2] = [512, 256];
/// Upper bound on the wall-clock interval between cloud-volume rebuilds. At
/// 1x simulation speed the 1 s model step dominates anyway (~1 Hz updates).
const VOLUME_INTERVAL: Duration = Duration::from_millis(280);
const MAX_WALL_STEP: f64 = 0.1;
/// At high time acceleration the baked wind advection moves the cloud field
/// tens to hundreds of metres between volume rebuilds; the renderer interpolates
/// that motion per frame, and the rebuild cadence is tightened so each update
/// shifts the field by at most this much (bounded to 10 Hz rebuilds).
const VOLUME_MAX_DRIFT_M: f32 = 60.0;

/// Mean wind (speed m/s, direction deg) of the cached weather diagnostics.
fn mean_wind(diagnostics: Option<&sky_weather::WeatherFrame>) -> (f32, f32) {
    diagnostics
        .map(|d| (d.wind_speed_m_s as f32, d.wind_dir_deg as f32))
        .unwrap_or((0.0, 0.0))
}

/// Wind "from" vector in world x/z (m/s): `speed * (sin(dir), cos(dir))`.
///
/// The weather model reports the direction the wind comes from. Both the
/// detail-noise drift and the base-volume sampling drift in `sky-clouds` use
/// `world + drift`; because the reconstructed cloud pattern travels downwind,
/// the drift that makes it advance downwind is exactly this "from" vector
/// (equivalently `-downwind`).
fn wind_from_vector(speed_m_s: f32, dir_deg: f32) -> [f32; 2] {
    let angle = dir_deg.to_radians();
    [angle.sin() * speed_m_s, angle.cos() * speed_m_s]
}

/// Wall-clock interval between volume rebuilds for the current time
/// acceleration. Higher `speed` moves the simulated cloud further per wall
/// second, so the cadence tightens to keep the per-update drift near
/// `VOLUME_MAX_DRIFT_M`; it is bounded to 0.10–.28 s so the CPU reconstruction
/// can never be asked for more than ~10 rebuilds/s.
fn volume_interval(wind_speed_m_s: f32, speed: f32) -> Duration {
    let effective = (wind_speed_m_s.max(0.0) * speed.max(0.0)).max(1.0e-3);
    let seconds = (VOLUME_MAX_DRIFT_M / effective).clamp(0.10, VOLUME_INTERVAL.as_secs_f32());
    Duration::from_secs_f32(seconds)
}

fn weather_config(controls: RealtimeControls) -> sky_weather::Config {
    let mut config = sky_weather::Config::default();
    config.mode = controls.weather_mode.core_mode(controls.weather_kind);
    config.climate = controls.weather_climate;
    config.season = controls.weather_season;
    config.latitude_deg = f64::from(controls.weather_latitude_deg);
    config.day_of_year = f64::from(controls.weather_day_of_year);
    config.start_hour_utc = f64::from(controls.weather_start_hour_utc);
    config.intensity = controls.weather_intensity;
    config.seed = controls.weather_seed;
    config.effects_quality = controls.weather_effects;
    config.auto_sun = controls.auto_sun;
    config.sun_elevation_deg = f64::from(controls.sun_elevation_deg);
    config.sun_azimuth_deg = f64::from(controls.sun_azimuth_deg);
    config
}

fn site_bits(controls: RealtimeControls) -> [u32; 3] {
    [
        controls.weather_latitude_deg.to_bits(),
        controls.weather_day_of_year.to_bits(),
        controls.weather_start_hour_utc.to_bits(),
    ]
}

fn cloud_config(controls: RealtimeControls, weather: &sky_weather::Config) -> sky_clouds::Config {
    let mut config = sky_clouds::Config::default();
    config.quality = match controls.cloud_quality {
        CloudQuality::Balanced => sky_clouds::Quality::Balanced,
        CloudQuality::High => sky_clouds::Quality::High,
    };
    // Both tiers now trace at a high fraction of the output resolution: the
    // previous 0.65x balanced trace left visible per-trace-pixel staircases on
    // distant cloud edges, which read as "voxel blocks" even with detail
    // erosion. 0.80x is the smallest scale where those steps stop being
    // obvious under the 5x5 bilateral upsample; high traces at 0.85x.
    config.trace_scale = match controls.cloud_quality {
        CloudQuality::Balanced => 0.8,
        CloudQuality::High => 0.85,
    };
    config.max_steps = match controls.cloud_quality {
        CloudQuality::Balanced => 32,
        CloudQuality::High => 40,
    };
    // Per-pixel stratified samples: 12 balanced / 16 high keeps the residual
    // per-pixel step noise low enough that TAA converges to a smooth edge
    // within the short warm-up used by captures.
    config.samples = match controls.cloud_quality {
        CloudQuality::Balanced => 12,
        CloudQuality::High => 16,
    };
    config.detail_strength = match controls.cloud_quality {
        CloudQuality::Balanced => 0.8,
        CloudQuality::High => 0.9,
    };
    // More sun-march steps sharpen self-shadowing. The shape now has an
    // extra high-frequency octave, so the balanced tier needs 6 steps to keep
    // the billows readable instead of flat white blobs.
    config.sun_steps = match controls.cloud_quality {
        CloudQuality::Balanced => 6,
        CloudQuality::High => 10,
    };
    // Less flat ambient fill: cloud bodies get their form from sun
    // self-shadowing instead of a uniform grey lift.
    config.ambient_scale = match controls.cloud_quality {
        CloudQuality::Balanced => 0.25,
        CloudQuality::High => 0.24,
    };
    // Aerial perspective: fade distant cloud radiance toward the sky over a
    // 30 km e-folding scale. Visual approximation; the background sky already
    // contains the physical atmosphere.
    config.aerial_distance_m = 12_000.0;
    config.cloud_shadow_strength = 0.9;
    config.extinction_scale = 1.35;
    config.volume_dims = weather.volume_dims;
    config
}

fn effects_config(controls: RealtimeControls) -> sky_weather_fx::Config {
    let mut config = sky_weather_fx::Config::default();
    config.quality = match controls.weather_effects {
        EffectsQuality::Low => sky_weather_fx::Quality::Low,
        EffectsQuality::High => sky_weather_fx::Quality::High,
    };
    config.max_effects = match controls.weather_effects {
        EffectsQuality::Low => 5,
        EffectsQuality::High => 10,
    };
    config
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppliedWeather {
    Auto,
    Fixed(WeatherKind),
    Tour,
}

impl AppliedWeather {
    fn from_controls(controls: RealtimeControls) -> Self {
        match controls.weather_mode {
            WeatherModeControl::Auto => Self::Auto,
            WeatherModeControl::Fixed => Self::Fixed(controls.weather_kind),
            WeatherModeControl::Tour => Self::Tour,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresentSource {
    Sky,
    Effects,
}

pub struct WeatherExperiment {
    renderer: Renderer,
    environment_renderer: Renderer,
    present: TexturePresentPass,
    reference: ReferenceTexture,
    wavelengths: Wavelengths,
    weather: sky_weather::Model,
    clouds: sky_clouds::Renderer,
    effects: sky_weather_fx::Renderer,
    controls: RealtimeControls,
    applied_weather: AppliedWeather,
    applied_seed: u64,
    applied_climate: sky_weather::Climate,
    applied_intensity_bits: u32,
    applied_season_bits: u32,
    applied_site_bits: [u32; 3],
    applied_quality: CloudQuality,
    applied_effects: EffectsQuality,
    last_step: Instant,
    last_volume: Instant,
    /// Weather clock at the last volume upload. Rebuilding the 1.57M-voxel
    /// volume when the model has not stepped (e.g. at 1x speed, where the
    /// model advances in 1 s steps) only adds a 13 ms CPU hitch without new
    /// data.
    last_volume_sim_time: f64,
    /// Background worker that reconstructs the 1.57 M-voxel cloud volume and
    /// encodes it to f16 off the render thread.
    volume_worker: VolumeWorker,
    /// Continuous simulated clock used for the detail-noise drift. The
    /// weather model only ticks in 1 s steps, so using its quantised clock
    /// would make the high-frequency cloud detail jump once per second at
    /// 1x speed; this clock advances every rendered frame instead.
    cloud_time_s: f64,
    environment_key: Option<(f32, f32, f32)>,
    sun_override: Option<(f32, f32)>,
    present_source: PresentSource,
    effects_background_valid: bool,
    applied_enabled: bool,
    error: Option<String>,
    diagnostics: Option<sky_weather::WeatherFrame>,
    /// Smoothed 0..1 overcast occlusion derived from coverage / LWP; sent to
    /// `sky-clouds` to dim the sky/ground and cloud radiance under thick decks.
    overcast: f32,
    /// Smoothed 0..1 convective storm severity; adds the extra near-dark drop.
    storm_severity: f32,
}

/// Convective storm severity (0..1) for the extra near-dark composite drop.
/// Driven by lightning, hail and very heavy rain so ordinary overcast/rain keep
/// the daylight overcast curve; it is an application-level visual coupling, not
/// a physical storm intensity metric.
fn storm_severity(frame: &sky_weather::WeatherFrame) -> f32 {
    let lightning = (frame.lightning_rate_per_min as f32 / 3.0).clamp(0.0, 1.0);
    let hail = (frame.precipitation.hail_fraction * 2.0).clamp(0.0, 1.0);
    let torrential = ((frame.precipitation.rate_mm_h as f32 - 30.0) / 60.0).clamp(0.0, 1.0);
    let convective = lightning.max(hail).max(torrential);
    if convective <= 0.0 {
        return 0.0;
    }
    // Very deep, water-loaded convective towers (supercell-like) push past 1.0
    // into the extra near-dark tier; ordinary thunderstorms stay at 1.0.
    let depth = ((frame.lwp_g_m2 as f32 - 2_200.0) / 800.0).clamp(0.0, 0.5);
    (convective + depth).clamp(0.0, 1.5)
}

/// Storm severity scaled by the workbench occlusion strength so the
/// 「云层遮光强度」slider / --cloud-occlusion dials back the whole
/// application-level darkening (including the supercell tier), not only the
/// plain overcast curve.
fn scaled_storm_severity(frame: &sky_weather::WeatherFrame, strength: f32) -> f32 {
    let strength = if strength.is_finite() {
        strength.clamp(0.0, 1.0)
    } else {
        1.0
    };
    storm_severity(frame) * strength
}

/// Weather-driven overcast occlusion for the cloud composite.
///
/// This is an application-level coupling between the behaviour model and the
/// cloud renderer: coverage sets how much of the sky is clouded, while the
/// liquid-water path sets how optically thick that deck is. It approximates the
/// missing transported cloud-shadow / diffuse-illumination field; it is not a
/// radiometric solve. `controls.cloud_occlusion` scales the user-facing knob.
fn overcast_occlusion(frame: &sky_weather::WeatherFrame, strength: f32) -> f32 {
    let cover = (frame.cloud_cover as f32).clamp(0.0, 1.0);
    let lwp_g_m2 = (frame.lwp_g_m2 as f32).max(0.0);
    let optical = 1.0 - (-lwp_g_m2 / 180.0).exp();
    let strength = if strength.is_finite() {
        strength.clamp(0.0, 1.0)
    } else {
        1.0
    };
    (cover * optical * strength).clamp(0.0, 1.0)
}

/// Everything the render thread needs to upload a reconstructed cloud volume.
///
/// Produced entirely on the volume worker: the f32 voxel data has already been
/// encoded to the half-precision bits stored in the `Rgba16Float` volume
/// texture, so resolving a finished rebuild on the render thread is one
/// `queue.write_texture` and no CPU volume maths.
struct VolumePayload {
    dims: [u32; 3],
    origin_m: [f32; 3],
    spacing_m: [f32; 3],
    half: Vec<u16>,
    /// Simulated weather time of the model snapshot that produced this volume.
    sim_time: f64,
    /// Behaviour diagnostics of the same snapshot; the render thread caches them
    /// for the HUD/overcast coupling instead of re-reducing the live field.
    diagnostics: sky_weather::WeatherFrame,
}

struct VolumeRequest {
    generation: u64,
    model: sky_weather::Model,
}

struct VolumeResponse {
    generation: u64,
    result: Result<VolumePayload, String>,
}

#[derive(Default)]
struct VolumeWorkerState {
    /// At most one queued request; a newer request replaces the queued one.
    request: Mutex<Option<VolumeRequest>>,
    /// At most one finished result waiting for the render thread.
    response: Mutex<Option<VolumeResponse>>,
    signal: Condvar,
    shutdown: AtomicBool,
}

/// Single background thread that reconstructs the weather cloud volume.
///
/// The worker executes at most one job at a time and owns at most one queued
/// request, so the render thread can never build an unbounded backlog: while a
/// job is pending the render thread skips redundant triggers, and a newer
/// request replaces a queued-but-not-started one. Results carry a generation
/// stamp; [`VolumeWorker::invalidate`] bumps it so a volume reconstructed from
/// an old weather state is discarded instead of uploaded.
///
/// The render thread only clones the model into [`VolumeWorker::submit`]
/// (bounded, ~1 ms) and polls [`VolumeWorker::take_matching_result`]; it never
/// waits for the worker.
struct VolumeWorker {
    state: Arc<VolumeWorkerState>,
    thread: Option<std::thread::JoinHandle<()>>,
    next_generation: u64,
    /// Generation the render thread is currently waiting for.
    pending: Option<u64>,
}

impl VolumeWorker {
    fn new() -> Result<Self, String> {
        let state = Arc::new(VolumeWorkerState::default());
        let worker_state = Arc::clone(&state);
        let thread = std::thread::Builder::new()
            .name("weather-volume".into())
            .spawn(move || volume_worker_loop(worker_state))
            .map_err(|error| format!("failed to spawn cloud-volume worker: {error}"))?;
        Ok(Self {
            state,
            thread: Some(thread),
            next_generation: 0,
            pending: None,
        })
    }

    fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Queue the given immutable weather snapshot for reconstruction. Cheap and
    /// non-blocking: it only moves the clone into the request slot and wakes the
    /// worker.
    fn submit(&mut self, model: sky_weather::Model) {
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        *lock(&self.state.request) = Some(VolumeRequest { generation, model });
        self.pending = Some(generation);
        self.state.signal.notify_one();
    }

    /// Disown the pending and any queued rebuild. A result from the old state is
    /// discarded by the generation check in [`Self::take_matching_result`].
    fn invalidate(&mut self) {
        self.next_generation = self.next_generation.wrapping_add(1);
        self.pending = None;
        *lock(&self.state.request) = None;
    }

    /// Take the finished payload for the pending generation, if it is ready.
    /// Stale results (from before an invalidation) are dropped.
    fn take_matching_result(&mut self) -> Option<Result<VolumePayload, String>> {
        while let Some(response) = lock(&self.state.response).take() {
            if self.pending == Some(response.generation) {
                self.pending = None;
                return Some(response.result);
            }
        }
        None
    }
}

impl Drop for VolumeWorker {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.state.shutdown.store(true, Ordering::Release);
            self.state.signal.notify_all();
            let _ = thread.join();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn volume_worker_loop(state: Arc<VolumeWorkerState>) {
    loop {
        let request = {
            let mut slot = lock(&state.request);
            loop {
                if state.shutdown.load(Ordering::Acquire) {
                    return;
                }
                if let Some(request) = slot.take() {
                    break request;
                }
                slot = state
                    .signal
                    .wait(slot)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        // A panic must not take down the render thread or wedge the pipeline:
        // turn it into an ordinary error result that surfaces through
        // `WeatherExperiment::error`.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            build_volume_payload(&request.model)
        }))
        .unwrap_or_else(|_| Err("cloud-volume worker panicked".to_owned()));
        *lock(&state.response) = Some(VolumeResponse {
            generation: request.generation,
            result,
        });
    }
}

fn build_volume_payload(model: &sky_weather::Model) -> Result<VolumePayload, String> {
    let volume = model.sample_volume().map_err(|error| error.to_string())?;
    let half = sky_clouds::encode_volume_half(&volume.data);
    Ok(VolumePayload {
        dims: volume.dims,
        origin_m: volume.origin_m,
        spacing_m: volume.spacing_m,
        half,
        sim_time: model.time_s(),
        diagnostics: model.diagnostics(),
    })
}

impl WeatherExperiment {
    pub fn new(context: ExperimentInit<'_>) -> Result<Self, String> {
        let model = sky_realtime::model::Model::earth().map_err(|e| e.to_string())?;
        let wavelengths = Wavelengths::optimized_four();
        let config = Config::balanced();
        let mut renderer =
            Renderer::new(context.device, &model, &wavelengths, 0.18, config.clone())
                .map_err(|e| e.to_string())?;
        renderer
            .rebuild(context.device, context.queue)
            .map_err(|e| e.to_string())?;
        renderer.include_sun_disk = true;

        let mut environment_renderer =
            Renderer::new(context.device, &model, &wavelengths, 0.18, config)
                .map_err(|e| e.to_string())?;
        environment_renderer
            .rebuild(context.device, context.queue)
            .map_err(|e| e.to_string())?;
        environment_renderer.resize(context.device, ENVIRONMENT_SIZE);

        let controls = RealtimeControls::from_asset(context.asset);
        let weather_config = weather_config(controls);
        let weather = sky_weather::Model::new(weather_config.clone()).map_err(|e| e.to_string())?;
        let clouds = sky_clouds::Renderer::new(
            context.device,
            context.queue,
            cloud_config(controls, &weather_config),
        )
        .map_err(|e| e.to_string())?;
        let effects =
            sky_weather_fx::Renderer::new(context.device, context.queue, effects_config(controls))
                .map_err(|e| e.to_string())?;
        let volume_worker = VolumeWorker::new()?;

        let reference = ReferenceTexture::from_asset(context.device, context.queue, context.asset);
        let present = TexturePresentPass::new(
            context.device,
            context.surface_format,
            effects.target_view(),
            &reference,
            context.display.exposure,
        );

        Ok(Self {
            renderer,
            environment_renderer,
            present,
            reference,
            wavelengths,
            weather,
            clouds,
            effects,
            controls,
            applied_weather: AppliedWeather::from_controls(controls),
            applied_seed: controls.weather_seed,
            applied_climate: controls.weather_climate,
            applied_intensity_bits: controls.weather_intensity.to_bits(),
            applied_season_bits: controls.weather_season.to_bits(),
            applied_site_bits: site_bits(controls),
            applied_quality: controls.cloud_quality,
            applied_effects: controls.weather_effects,
            last_step: Instant::now(),
            last_volume: Instant::now() - VOLUME_INTERVAL,
            last_volume_sim_time: -1.0,
            volume_worker,
            cloud_time_s: 0.0,
            environment_key: None,
            sun_override: None,
            present_source: PresentSource::Effects,
            effects_background_valid: false,
            applied_enabled: true,
            error: None,
            diagnostics: None,
            overcast: 0.0,
            storm_severity: 0.0,
        })
    }

    fn set_error(&mut self, error: impl Into<String>) {
        let error = error.into();
        if self.error.as_deref() != Some(error.as_str()) {
            eprintln!("weather experiment: {error}");
        }
        self.error = Some(error);
    }

    /// Update the smoothed weather occlusion used by the sky pass (sun disk
    /// gain) and the cloud composite. Called once per frame before rendering.
    fn update_overcast(&mut self, controls: RealtimeControls) {
        let target_overcast = if controls.weather_enabled {
            self.diagnostics.as_ref().map_or(0.0, |frame| {
                overcast_occlusion(frame, controls.cloud_occlusion)
            })
        } else {
            0.0
        };
        self.overcast = if self.overcast <= 0.0 {
            target_overcast
        } else {
            self.overcast * 0.82 + target_overcast * 0.18
        };
        let target_severity = if controls.weather_enabled {
            self.diagnostics.as_ref().map_or(0.0, |frame| {
                scaled_storm_severity(frame, controls.cloud_occlusion)
            })
        } else {
            0.0
        };
        self.storm_severity = if self.storm_severity <= 0.0 {
            target_severity
        } else {
            self.storm_severity * 0.82 + target_severity * 0.18
        };
    }

    /// Rebuild the behaviour model immediately (used by headless snapshots).
    pub fn configure_weather(&mut self, controls: RealtimeControls) -> Result<(), String> {
        self.controls = controls;
        let config = weather_config(controls);
        let model = sky_weather::Model::new(config).map_err(|e| e.to_string())?;
        self.weather = model;
        // A volume being reconstructed from the replaced weather state must
        // never reach the GPU; the next frame submits the new state instead.
        self.volume_worker.invalidate();
        // The weather state behind the volume was replaced; old temporal
        // history would ghost the previous weather into the new cloud field.
        self.clouds.reset_history();
        self.applied_weather = AppliedWeather::from_controls(controls);
        self.applied_seed = controls.weather_seed;
        self.applied_climate = controls.weather_climate;
        self.applied_intensity_bits = controls.weather_intensity.to_bits();
        self.applied_season_bits = controls.weather_season.to_bits();
        self.applied_site_bits = site_bits(controls);
        self.last_step = Instant::now();
        self.last_volume = Instant::now() - VOLUME_INTERVAL;
        self.last_volume_sim_time = -1.0;
        self.cloud_time_s = 0.0;
        self.sun_override = None;
        self.environment_key = None;
        self.effects_background_valid = false;
        self.diagnostics = None;
        self.overcast = 0.0;
        self.storm_severity = 0.0;
        self.error = None;
        Ok(())
    }

    /// Advance the behaviour model without rendering, for snapshot spin-up.
    pub fn spin_up_weather(&mut self, seconds: f64) -> Result<(), String> {
        if !seconds.is_finite() || !(0.0..=86_400.0).contains(&seconds) {
            return Err("weather spin-up must be finite and within 0..=86400 s".into());
        }
        let mut remaining = seconds;
        while remaining > 1.0e-9 {
            let chunk = remaining.min(600.0);
            self.weather.advance(chunk).map_err(|e| e.to_string())?;
            remaining -= chunk;
        }
        self.volume_worker.invalidate();
        self.diagnostics = Some(self.weather.diagnostics());
        self.last_volume = Instant::now() - VOLUME_INTERVAL;
        self.last_volume_sim_time = -1.0;
        self.cloud_time_s = self.weather.time_s();
        Ok(())
    }

    /// Hand the current weather state to the background volume worker.
    fn submit_volume_rebuild(&mut self) {
        self.volume_worker.submit(self.weather.clone());
    }

    /// Upload a volume the worker finished since the last frame. This is the
    /// only part of the rebuild that still runs on the render thread: validation
    /// plus `queue.write_texture`, with no sampling or f32->f16 conversion.
    fn upload_finished_volume(&mut self, queue: &wgpu::Queue) {
        let Some(result) = self.volume_worker.take_matching_result() else {
            return;
        };
        match result {
            Ok(payload) => {
                let descriptor = sky_clouds::VolumeDescriptor {
                    dims: payload.dims,
                    origin_m: payload.origin_m,
                    spacing_m: payload.spacing_m,
                };
                match self
                    .clouds
                    .upload_volume_half(queue, &payload.half, descriptor)
                {
                    Ok(()) => {
                        self.last_volume = Instant::now();
                        self.last_volume_sim_time = payload.sim_time;
                        self.diagnostics = Some(payload.diagnostics);
                    }
                    Err(error) => self.set_error(error.to_string()),
                }
            }
            Err(error) => self.set_error(error),
        }
    }

    /// Synchronous rebuild used only to establish the first volume after a
    /// cloud-renderer (re)creation: a renderer without an uploaded volume draws
    /// an empty cloud field, so the first presented frame would be wrong. Every
    /// steady-state rebuild is asynchronous.
    fn rebuild_volume_blocking(&mut self, queue: &wgpu::Queue) -> Result<(), String> {
        let volume = self.weather.sample_volume().map_err(|e| e.to_string())?;
        let descriptor = sky_clouds::VolumeDescriptor {
            dims: volume.dims,
            origin_m: volume.origin_m,
            spacing_m: volume.spacing_m,
        };
        self.clouds
            .upload_volume(queue, &volume.data, descriptor)
            .map_err(|e| e.to_string())?;
        self.last_volume = Instant::now();
        self.last_volume_sim_time = self.weather.time_s();
        self.diagnostics = Some(self.weather.diagnostics());
        Ok(())
    }

    fn update_environment(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: View,
    ) {
        let key = (
            view.sun_elevation_deg,
            view.sun_azimuth_deg,
            view.altitude_km,
        );
        if self.environment_key == Some(key) {
            return;
        }
        self.environment_key = Some(key);
        self.environment_renderer
            .render_environment(queue, encoder, view);
        let transmittance = self
            .environment_renderer
            .transmittance_texture()
            .create_view(&Default::default());
        let weights = std::array::from_fn(|j| {
            std::array::from_fn(|k| self.wavelengths.solar[k] * self.wavelengths.rgb[k][j])
        });
        self.clouds.set_environment(
            device,
            self.environment_renderer.target_view(),
            &transmittance,
            weights,
        );
    }

    fn present_target_for(&self, controls: RealtimeControls) -> PresentSource {
        if !controls.weather_enabled {
            PresentSource::Sky
        } else {
            PresentSource::Effects
        }
    }

    fn sync_present_source(&mut self, device: &wgpu::Device, desired: PresentSource, force: bool) {
        if !force && self.present_source == desired {
            return;
        }
        let view = match desired {
            PresentSource::Sky => self.renderer.target_view(),
            PresentSource::Effects => self.effects.target_view(),
        };
        self.present.set_source(device, view, &self.reference);
        self.present_source = desired;
    }
}

impl RealtimeExperiment for WeatherExperiment {
    fn name(&self) -> &'static str {
        "行为天气"
    }

    fn set_linear_output(&mut self, enabled: bool) {
        self.present.set_linear_output(enabled);
    }

    fn update(&mut self, context: UpdateContext<'_>) {
        self.controls = *context.controls;
    }

    fn warm_up(&mut self, sim_seconds: f64) -> Result<(), String> {
        self.spin_up_weather(sim_seconds)
    }

    fn configure(&mut self, controls: &RealtimeControls) -> Result<(), String> {
        self.configure_weather(*controls)
    }

    fn weather_hud(&self) -> Option<WeatherHud> {
        let d = self.diagnostics.as_ref()?;
        Some(WeatherHud {
            kind: weather_kind_label(d.kind),
            temperature_c: d.temperature_c as f32,
            dew_point_c: d.dew_point_c as f32,
            humidity: d.humidity as f32,
            cloud_cover: d.cloud_cover as f32,
            lwp_g_m2: d.lwp_g_m2 as f32,
            cloud_base_m: d.cloud_base_m as f32,
            cloud_top_m: d.cloud_top_m as f32,
            precipitation_kind: precipitation_kind_label(d.precipitation.kind),
            precipitation_rate_mm_h: d.precipitation.rate_mm_h as f32,
            wind_speed_m_s: d.wind_speed_m_s as f32,
            wind_dir_deg: d.wind_dir_deg as f32,
            visibility_m: d.visibility_m as f32,
            lightning_rate_per_min: d.lightning_rate_per_min as f32,
            rainbow: d.rainbow_strength as f32,
            fog: d.fog_density as f32,
            dust: d.dust_density as f32,
            sun_elevation_deg: d.sun.elevation_deg as f32,
            sun_azimuth_deg: d.sun.azimuth_deg as f32,
            sim_time_s: d.time_s,
        })
    }

    fn reference_available(&self) -> bool {
        false
    }

    fn render(&mut self, context: FrameContext<'_>) {
        let controls = self.controls;
        let size = [
            context.viewport.width.max(1),
            context.viewport.height.max(1),
        ];

        let applied_weather = AppliedWeather::from_controls(controls);
        if applied_weather != self.applied_weather
            || controls.weather_seed != self.applied_seed
            || controls.weather_climate != self.applied_climate
            || controls.weather_intensity.to_bits() != self.applied_intensity_bits
            || controls.weather_season.to_bits() != self.applied_season_bits
            || site_bits(controls) != self.applied_site_bits
        {
            let config = weather_config(controls);
            match sky_weather::Model::new(config) {
                Ok(model) => {
                    self.weather = model;
                    self.clouds.reset_history();
                    // Discard any volume being built from the replaced state.
                    self.volume_worker.invalidate();
                    self.applied_weather = applied_weather;
                    self.applied_seed = controls.weather_seed;
                    self.applied_climate = controls.weather_climate;
                    self.applied_intensity_bits = controls.weather_intensity.to_bits();
                    self.applied_season_bits = controls.weather_season.to_bits();
                    self.applied_site_bits = site_bits(controls);
                    self.last_step = Instant::now();
                    self.last_volume = Instant::now() - VOLUME_INTERVAL;
                    self.last_volume_sim_time = -1.0;
                    self.sun_override = None;
                    self.environment_key = None;
                    self.diagnostics = None;
                    self.overcast = 0.0;
                    self.storm_severity = 0.0;
                    self.effects_background_valid = false;
                    self.error = None;
                }
                Err(error) => self.set_error(error.to_string()),
            }
        }

        if controls.cloud_quality != self.applied_quality {
            let config = cloud_config(controls, self.weather.config());
            match sky_clouds::Renderer::new(context.device, context.queue, config) {
                Ok(renderer) => {
                    self.clouds = renderer;
                    self.applied_quality = controls.cloud_quality;
                    // The new renderer starts without an uploaded volume; drop
                    // any payload in flight for the replaced renderer.
                    self.volume_worker.invalidate();
                    self.last_volume = Instant::now() - VOLUME_INTERVAL;
                    self.last_volume_sim_time = -1.0;
                    self.environment_key = None;
                    self.effects_background_valid = false;
                    self.error = None;
                }
                Err(error) => self.set_error(error.to_string()),
            }
        }

        if controls.weather_effects != self.applied_effects {
            match sky_weather_fx::Renderer::new(
                context.device,
                context.queue,
                effects_config(controls),
            ) {
                Ok(renderer) => {
                    self.effects = renderer;
                    self.applied_effects = controls.weather_effects;
                    self.effects_background_valid = false;
                    self.present_source = PresentSource::Sky;
                    self.error = None;
                }
                Err(error) => self.set_error(error.to_string()),
            }
        }

        let now = Instant::now();
        let wall_step = now
            .duration_since(self.last_step)
            .as_secs_f64()
            .clamp(0.0, MAX_WALL_STEP);
        self.last_step = now;
        if controls.weather_enabled && !controls.weather_paused && self.error.is_none() {
            let sim_seconds = wall_step * f64::from(controls.weather_speed);
            if sim_seconds > 0.0 {
                self.cloud_time_s += sim_seconds;
                if let Err(error) = self.weather.advance(sim_seconds) {
                    self.set_error(format!("weather advance failed: {error}"));
                }
            }
        }

        if !controls.auto_sun {
            let override_sun = (controls.sun_elevation_deg, controls.sun_azimuth_deg);
            if self.sun_override != Some(override_sun) {
                if let Err(error) = self
                    .weather
                    .set_sun(f64::from(override_sun.0), f64::from(override_sun.1))
                {
                    self.set_error(format!("sun override failed: {error}"));
                }
                self.sun_override = Some(override_sun);
            }
        } else if self.sun_override.is_some() {
            self.sun_override = None;
        }

        let sun = self.weather.sun();
        let view = View {
            yaw_deg: controls.view.yaw_deg,
            pitch_deg: controls.view.pitch_deg,
            fov_y_deg: controls.view.fov_y_deg,
            sun_azimuth_deg: sun.azimuth_deg as f32,
            sun_elevation_deg: sun.elevation_deg as f32,
            altitude_km: controls.observer_altitude_km,
        };
        let environment_view = View {
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            fov_y_deg: 90.0,
            ..view
        };

        let screen_resized = self.renderer.resize(context.device, size);
        let cloud_resized = self.clouds.resize(context.device, size);
        let effects_resized = self.effects.resize(context.device, size);
        let target_changed = screen_resized || cloud_resized || effects_resized;

        self.update_overcast(controls);
        // Thick overcast decks hide the direct sun disk as well; the gain is a
        // display-level fade, not a transported cloud shadow.
        self.renderer.sun_disk_gain = (1.0 - 1.15 * self.overcast).clamp(0.0, 1.0);
        self.renderer.render(context.queue, context.encoder, view);

        // Volume reconstruction (sample_volume) and the f32->f16 encode run on
        // the background worker. Here the render thread only polls for a
        // finished payload and issues the GPU upload, so the periodic rebuild no
        // longer blocks a frame. At most one rebuild is in flight.
        if controls.weather_enabled && self.error.is_none() {
            self.upload_finished_volume(context.queue);
            if self.error.is_none() && !self.clouds.has_volume() {
                // Startup and cloud-quality changes replace the renderer, which
                // starts from an empty volume. Build that first volume
                // synchronously so the first presented frame is correct; every
                // steady-state rebuild is asynchronous.
                if let Err(error) = self.rebuild_volume_blocking(context.queue) {
                    self.set_error(error);
                }
            }
        } else if !controls.weather_enabled && self.volume_worker.has_pending() {
            // Weather was switched off while a rebuild was in flight: discard it
            // so a volume from the old state can never be uploaded.
            self.volume_worker.invalidate();
        }

        let weather_time = self.weather.time_s();
        let (mean_wind_speed, _) = mean_wind(self.diagnostics.as_ref());
        let rebuild_interval = volume_interval(mean_wind_speed, controls.weather_speed);
        let needs_volume = controls.weather_enabled
            && self.error.is_none()
            && self.clouds.has_volume()
            && !self.volume_worker.has_pending()
            && now.duration_since(self.last_volume) >= rebuild_interval
            && weather_time > self.last_volume_sim_time + 1.0e-9;
        if needs_volume {
            // Sampling, LWP normalisation and f16 encoding all happen on the
            // worker; the render thread only keeps the snapshot and polls.
            self.submit_volume_rebuild();
        }

        if controls.weather_enabled {
            self.update_environment(
                context.device,
                context.queue,
                context.encoder,
                environment_view,
            );
            self.clouds
                .set_background(context.device, self.renderer.target_view());
            self.clouds.set_overcast(self.overcast);
            self.clouds.set_storm_severity(self.storm_severity);
            let camera = sky_clouds::Camera {
                origin_m: [0.0, controls.observer_altitude_km * 1000.0, 0.0],
                yaw_deg: controls.view.yaw_deg,
                pitch_deg: controls.view.pitch_deg,
                fov_y_deg: controls.view.fov_y_deg,
            };
            // The sub-grid cloud detail noise is advected by the same mean
            // wind as the weather volume, so it travels with the cloud instead
            // of swimming through it as the simulation clock advances. Wind is
            // taken from the cached diagnostics (refreshed with the volume at
            // ~1-10 Hz); calling frame() every frame would add a full column
            // reduction to every presented frame for no visible gain.
            let (wind_speed, wind_dir_deg) = mean_wind(self.diagnostics.as_ref());
            let wind_from = wind_from_vector(wind_speed, wind_dir_deg);
            let sim_time = self.cloud_time_s as f32;
            // Detail texture sampling uses `world + drift`, so a pattern that
            // moves downwind needs drift = wind_from * t.
            self.clouds
                .set_detail_drift([wind_from[0] * sim_time, wind_from[1] * sim_time]);
            // The uploaded volume is a snapshot of the cloud field at
            // `last_volume_sim_time`; the reconstructed data already contains
            // the baked advection up to that time. Slide the volume sampling by
            // the downwind displacement accumulated since then so cloud motion
            // is continuous at frame rate instead of jumping at the rebuild
            // cadence. The same offset resets to zero each time a new snapshot
            // is uploaded, so it stays small (bounded by the rebuild interval).
            let since_upload = if self.last_volume_sim_time >= 0.0 {
                (self.cloud_time_s - self.last_volume_sim_time).max(0.0) as f32
            } else {
                0.0
            };
            self.clouds
                .set_volume_drift([wind_from[0] * since_upload, wind_from[1] * since_upload]);
            let elevation = (sun.elevation_deg as f32).to_radians();
            let azimuth = (sun.azimuth_deg as f32).to_radians();
            let sun_direction = [
                azimuth.sin() * elevation.cos(),
                elevation.sin(),
                azimuth.cos() * elevation.cos(),
            ];
            let sun = sky_clouds::Sun {
                direction: sun_direction,
            };
            if let Err(error) = self
                .clouds
                .render(context.queue, context.encoder, camera, sun)
            {
                self.set_error(format!("cloud render failed: {error}"));
            }

            if !self.effects_background_valid || cloud_resized || effects_resized {
                self.effects
                    .set_background(context.device, self.clouds.target_view());
                self.effects_background_valid = true;
            }
            let state = self.weather.effects();
            let input = sky_weather_fx::Input {
                time_s: state.time_s,
                camera: sky_weather_fx::Camera {
                    origin_m: [0.0, controls.observer_altitude_km * 1000.0, 0.0],
                    yaw_deg: controls.view.yaw_deg,
                    pitch_deg: controls.view.pitch_deg,
                    fov_y_deg: controls.view.fov_y_deg,
                },
                sun: sky_weather_fx::Sun {
                    direction: sun_direction,
                },
                rain: state.rain,
                snow: state.snow,
                hail: state.hail,
                sleet: state.sleet,
                fog: state.fog,
                dust: state.dust,
                wind: state.wind,
                cloud_darkening: state.cloud_darkening,
                rainbow: state.rainbow,
                lightning_flash: state.lightning_flash,
                lightning_seed: state.lightning_seed,
            };
            if let Err(error) = self.effects.render(context.queue, context.encoder, input) {
                self.set_error(format!("weather effects failed: {error}"));
            }
        }

        if controls.weather_enabled != self.applied_enabled || target_changed {
            self.sync_present_source(context.device, self.present_target_for(controls), true);
            self.applied_enabled = controls.weather_enabled;
        }

        self.present.update_uniform(
            context.queue,
            crate::experiment::CompareMode::Realtime,
            controls.view,
            size[0],
            size[1],
            false,
            controls.exposure_multiplier(),
            controls.difference_scale,
            controls.tone_mapping_enabled,
            controls.hdr_enabled,
            controls.reinhard_overexposure,
            controls.hdr_paper_white_scale(),
            controls.hdr_peak_scale(),
        );
        self.present
            .render(context.encoder, context.target, context.viewport);
    }

    fn status_line(&self) -> Option<String> {
        let d = self.diagnostics.as_ref()?;
        Some(format!(
            "{}  气温 {:>5.1}°C  云量 {:>3.0}%  LWP {:>5.1} g/m²  云底/云顶 {:>4.0}/{:>4.0} m  \
             {} {:>4.1} mm/h  风{:>4.1} m/s  能见度{:>5.0} m  闪电 {:>4.1}/min  t {:>5.0} s",
            weather_kind_label(d.kind),
            d.temperature_c,
            d.cloud_cover * 100.0,
            d.lwp_g_m2,
            d.cloud_base_m,
            d.cloud_top_m,
            precipitation_kind_label(d.precipitation.kind),
            d.precipitation.rate_mm_h,
            d.wind_speed_m_s,
            d.visibility_m,
            d.lightning_rate_per_min,
            d.time_s
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(cover: f64, lwp: f64) -> sky_weather::WeatherFrame {
        sky_weather::WeatherFrame {
            cloud_cover: cover,
            lwp_g_m2: lwp,
            ..sky_weather::WeatherFrame::default()
        }
    }

    fn small_weather_model(kind: WeatherKind) -> sky_weather::Model {
        let mut config = sky_weather::Config::fixed(kind);
        config.grid = [16, 16];
        config.volume_dims = [8, 8, 8];
        config.volume_extent_m = [16_000.0, 8_000.0, 16_000.0];
        let mut model = sky_weather::Model::new(config).expect("weather model");
        model.advance(60.0).expect("advance");
        model
    }

    fn wait_for_payload(worker: &mut VolumeWorker) -> VolumePayload {
        loop {
            if let Some(result) = worker.take_matching_result() {
                return result.expect("worker volume");
            }
            std::thread::yield_now();
        }
    }

    #[test]
    fn volume_rebuild_interval_tightens_with_time_acceleration() {
        // 1x: the 1 s weather-model clock dominates; the interval is capped at
        // the historical 280 ms.
        assert!((volume_interval(5.0, 1.0).as_secs_f32() - 0.28).abs() < 1.0e-4);
        // 60x and 5 m/s: 60 m of drift per rebuild -> 200 ms.
        assert!((volume_interval(5.0, 60.0).as_secs_f32() - 0.2).abs() < 1.0e-3);
        // Extreme acceleration is floored at 10 rebuilds/s.
        assert!((volume_interval(5.0, 600.0).as_secs_f32() - 0.1).abs() < 1.0e-4);
        // Calm air: no extra rebuild rate.
        assert!((volume_interval(0.0, 600.0).as_secs_f32() - 0.28).abs() < 1.0e-4);
        // Non-finite wind must not produce a NaN/negative Duration.
        assert!(volume_interval(f32::NAN, 60.0).as_secs_f32() > 0.09);
    }

    #[test]
    fn wind_from_drift_points_upwind_of_the_reported_direction() {
        // Wind from the west (270 deg) blows toward +x; the visual drift that
        // advances the downwind pattern under `world + drift` is -x.
        let drift = wind_from_vector(4.0, 270.0);
        assert!((drift[0] + 4.0).abs() < 1.0e-5, "{drift:?}");
        assert!(drift[1].abs() < 1.0e-5, "{drift:?}");
        // Wind from the north (0 deg) blows toward -z; visual drift is +z.
        let drift = wind_from_vector(2.0, 0.0);
        assert!(drift[0].abs() < 1.0e-5, "{drift:?}");
        assert!((drift[1] - 2.0).abs() < 1.0e-5, "{drift:?}");
    }

    #[test]
    fn worker_produces_exactly_the_render_thread_volume() {
        let model = small_weather_model(WeatherKind::Rain);
        let expected = model.sample_volume().expect("direct volume");
        let expected_half = sky_clouds::encode_volume_half(&expected.data);

        let mut worker = VolumeWorker::new().expect("worker");
        worker.submit(model.clone());
        assert!(worker.has_pending());
        let payload = wait_for_payload(&mut worker);

        assert_eq!(payload.dims, expected.dims);
        assert_eq!(payload.origin_m, expected.origin_m);
        assert_eq!(payload.spacing_m, expected.spacing_m);
        assert_eq!(payload.half, expected_half, "worker f16 payload differs");
        assert_eq!(payload.sim_time, model.time_s());
        assert_eq!(payload.diagnostics.time_s, model.time_s());
        assert!(!worker.has_pending());
    }

    #[test]
    fn invalidated_rebuild_is_discarded_and_newer_request_wins() {
        let mut worker = VolumeWorker::new().expect("worker");
        worker.submit(small_weather_model(WeatherKind::Rain));
        // Invalidate before the reconstruction can be resolved: the rain volume
        // must never be handed back as the current generation.
        worker.invalidate();
        assert!(!worker.has_pending());
        worker.submit(small_weather_model(WeatherKind::Clear));
        let payload = wait_for_payload(&mut worker);
        // A clear-sky volume is all zeros; a stale rain volume would not be.
        assert!(
            payload.half.iter().all(|&bits| bits == 0),
            "stale pre-invalidation volume was uploaded"
        );
    }

    #[test]
    fn clear_sky_has_no_occlusion() {
        assert_eq!(overcast_occlusion(&frame(0.0, 0.0), 1.0), 0.0);
    }

    #[test]
    fn thick_storm_deck_is_strongly_occluding() {
        let storm = overcast_occlusion(&frame(0.93, 1921.0), 1.0);
        assert!(storm > 0.9, "{storm}");
        let overcast = overcast_occlusion(&frame(0.93, 206.0), 1.0);
        assert!(overcast > 0.5 && overcast < storm, "{overcast}");
        let partly = overcast_occlusion(&frame(0.47, 189.0), 1.0);
        assert!(partly < overcast, "{partly}");
    }

    #[test]
    fn occlusion_strength_scales_storm_severity_too() {
        let mut storm = frame(0.93, 1921.0);
        storm.lightning_rate_per_min = 9.0;
        assert_eq!(super::scaled_storm_severity(&storm, 0.0), 0.0);
        let full = super::scaled_storm_severity(&storm, 1.0);
        let half = super::scaled_storm_severity(&storm, 0.5);
        assert!(full > 0.0);
        assert!((half - full * 0.5).abs() < 1.0e-6);
    }

    #[test]
    fn strength_scales_and_clamps_the_occlusion() {
        let storm = frame(0.93, 1921.0);
        assert_eq!(overcast_occlusion(&storm, 0.0), 0.0);
        assert_eq!(
            overcast_occlusion(&storm, 1.0),
            overcast_occlusion(&storm, 1.0)
        );
        assert_eq!(
            overcast_occlusion(&storm, f32::NAN),
            overcast_occlusion(&storm, 1.0)
        );
        assert!(overcast_occlusion(&storm, 0.5) < overcast_occlusion(&storm, 1.0));
    }
}
