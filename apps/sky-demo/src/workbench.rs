use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Color32, RichText};

use crate::assets::RealtimeAsset;
use crate::catalog::{AssetEntry, CatalogScan};
use crate::controls::{
    Climate, CloudQuality, DEFAULT_HDR_PAPER_WHITE_NITS, DEFAULT_HDR_PEAK_NITS,
    DEFAULT_REINHARD_OVEREXPOSURE, EffectsQuality, MAX_EXPOSURE_EV, MAX_HDR_PAPER_WHITE_NITS,
    MAX_HDR_PEAK_NITS, MAX_REINHARD_OVEREXPOSURE, MAX_WEATHER_SPEED, MIN_EXPOSURE_EV,
    MIN_HDR_PAPER_WHITE_NITS, MIN_HDR_PEAK_NITS, MIN_REINHARD_OVEREXPOSURE, MIN_WEATHER_SPEED,
    RealtimeControls, WeatherKind, WeatherModeControl, climate_label, effects_quality_label,
    weather_kind_label,
};
use crate::experiment::{CompareMode, WeatherHud};
use crate::view::ViewState;

const PANEL_FILL: Color32 = Color32::from_rgb(8, 12, 16);
const TOOLBAR_FILL: Color32 = Color32::from_rgb(11, 17, 22);
const BORDER: Color32 = Color32::from_rgb(35, 49, 58);
const MUTED: Color32 = Color32::from_rgb(137, 154, 164);
const ACCENT: Color32 = Color32::from_rgb(65, 151, 211);
const ERROR: Color32 = Color32::from_rgb(224, 94, 94);
const WARNING: Color32 = Color32::from_rgb(219, 174, 82);
/// Readable body text on the translucent weather HUD overlay.
const HUD_TEXT: Color32 = Color32::from_rgb(214, 226, 233);
/// De-emphasised interaction hints / secondary HUD text.
const HUD_HINT: Color32 = Color32::from_rgb(130, 153, 167);
const HUD_FILL: Color32 = Color32::from_rgba_premultiplied(3, 6, 8, 208);
const HUD_BORDER: Color32 = Color32::from_rgba_premultiplied(18, 26, 31, 140);
const HUD_HINT_FILL: Color32 = Color32::from_rgba_premultiplied(2, 5, 7, 176);

/// Which display shell the workbench is presenting. The reference/PT
/// comparison layout and the real-time weather layout share the same
/// immediate-mode workbench but expose different controls and diagnostics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorkbenchMode {
    #[default]
    Reference,
    Weather,
}

/// Per-frame, read-only status passed from the application shell into the UI.
#[derive(Clone, Debug, Default)]
pub struct WorkbenchStatus {
    pub fps: f32,
    pub experiment_name: &'static str,
    pub weather: Option<WeatherHud>,
    /// True while rendering the offscreen `--smoke` capture loop, whose frame
    /// pacing is uncapped and therefore not the interactive frame rate.
    pub capture: bool,
}

#[derive(Clone, Debug)]
pub enum WorkbenchAction {
    OpenAsset,
    ChooseAssetRoot,
    RefreshCatalog,
    LoadAsset(PathBuf),
}

#[derive(Debug)]
pub struct WorkbenchFrame {
    pub viewport_points: egui::Rect,
    pub actions: Vec<WorkbenchAction>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusKind {
    Info,
    Error,
}

#[derive(Debug)]
pub struct WorkbenchState {
    pub controls: RealtimeControls,
    pub mode: WorkbenchMode,
    pub baked_atmosphere: Option<[f32; 3]>,
    pub hybrid_atmosphere: bool,
    pub weather_controls_visible: bool,
    hybrid_draft: [f32; 2],
    asset_root: PathBuf,
    entries: Vec<AssetEntry>,
    current_path: PathBuf,
    filter: String,
    assets_open: bool,
    inspector_open: bool,
    inspector_width: f32,
    ground_albedo_linked: bool,
    scanning: bool,
    scan_generation: u64,
    status: Option<(StatusKind, String)>,
}

impl WorkbenchState {
    pub fn new(asset: &RealtimeAsset, mode: WorkbenchMode) -> Self {
        let current_path = normalized_path(asset.manifest_path());
        let asset_root = current_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_owned();
        let weather = matches!(mode, WorkbenchMode::Weather);
        Self {
            controls: RealtimeControls::from_asset(asset),
            mode,
            baked_atmosphere: None,
            hybrid_atmosphere: false,
            weather_controls_visible: weather,
            hybrid_draft: [1.0, 0.18],
            entries: if asset.is_default_view() {
                Vec::new()
            } else {
                vec![AssetEntry::from_path(current_path.clone(), &asset_root)]
            },
            current_path,
            asset_root,
            filter: String::new(),
            assets_open: !weather,
            inspector_open: true,
            inspector_width: 340.0,
            ground_albedo_linked: true,
            scanning: false,
            scan_generation: 0,
            status: None,
        }
    }

    pub fn asset_root(&self) -> &Path {
        &self.asset_root
    }

    pub fn set_asset_root(&mut self, root: PathBuf) {
        self.asset_root = normalized_path(&root);
    }

    pub fn begin_scan(&mut self) -> (u64, PathBuf) {
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.scanning = true;
        self.status = Some((
            StatusKind::Info,
            format!("Scanning {}...", display_path(&self.asset_root)),
        ));
        (self.scan_generation, self.asset_root.clone())
    }

    pub fn apply_scan(&mut self, generation: u64, result: Result<CatalogScan, String>) {
        if generation != self.scan_generation {
            return;
        }
        self.scanning = false;
        match result {
            Ok(scan) => {
                self.asset_root = scan.root;
                self.entries = scan.entries;
                let invalid = self
                    .entries
                    .iter()
                    .filter(|entry| !entry.is_valid())
                    .count();
                self.status = Some((
                    StatusKind::Info,
                    format!(
                        "Found {} assets{}.",
                        self.entries.len().saturating_sub(invalid),
                        if invalid == 0 {
                            String::new()
                        } else {
                            format!("; {invalid} invalid")
                        }
                    ),
                ));
            }
            Err(error) => self.status = Some((StatusKind::Error, error)),
        }
    }

    pub fn set_current_asset(&mut self, asset: &RealtimeAsset) {
        self.current_path = normalized_path(asset.manifest_path());
        self.controls.sync_asset_bound(asset);
        if !self
            .entries
            .iter()
            .any(|entry| normalized_path(&entry.path) == self.current_path)
        {
            self.entries.push(AssetEntry::from_path(
                self.current_path.clone(),
                &self.asset_root,
            ));
            self.entries
                .sort_by_key(|entry| entry.path.to_string_lossy().to_lowercase());
        }
        self.status = Some((
            StatusKind::Info,
            format!("Loaded {}", display_path(&self.current_path)),
        ));
    }

    pub fn set_error(&mut self, message: impl Into<String>) {
        self.status = Some((StatusKind::Error, message.into()));
    }

    pub fn step_asset(&self, delta: i32) -> Option<PathBuf> {
        let visible = self.filtered_indices();
        if visible.len() < 2 {
            return None;
        }
        let current = visible
            .iter()
            .position(|index| normalized_path(&self.entries[*index].path) == self.current_path);
        let next = match (current, delta.signum()) {
            (Some(index), -1) => (index + visible.len() - 1) % visible.len(),
            (Some(index), _) => (index + 1) % visible.len(),
            (None, -1) => visible.len() - 1,
            (None, _) => 0,
        };
        Some(self.entries[visible[next]].path.clone())
    }

    pub fn show(
        &mut self,
        root: &mut egui::Ui,
        asset: &RealtimeAsset,
        reference_available: bool,
        hdr_supported: bool,
        hdr_active: bool,
        status: &WorkbenchStatus,
    ) -> WorkbenchFrame {
        let mut actions = Vec::new();

        self.top_toolbar(
            root,
            asset,
            reference_available,
            hdr_supported,
            &mut actions,
        );
        self.status_bar(
            root,
            asset,
            reference_available,
            hdr_supported,
            hdr_active,
            status,
        );
        if self.assets_open && self.mode == WorkbenchMode::Reference {
            self.assets_panel(root, &mut actions);
        }
        let remaining = root.available_rect_before_wrap();
        let viewport_points = if self.inspector_open {
            let maximum_width = (remaining.width() - 320.0).clamp(280.0, 480.0);
            self.inspector_width = self.inspector_width.clamp(280.0, maximum_width);

            let divider_x = remaining.max.x - self.inspector_width;
            let divider = egui::Rect::from_min_max(
                egui::pos2(divider_x - 3.0, remaining.min.y),
                egui::pos2(divider_x + 3.0, remaining.max.y),
            );
            let resize = root.interact(
                divider,
                root.make_persistent_id("workbench-inspector-resize"),
                egui::Sense::drag(),
            );
            if resize.dragged() {
                self.inspector_width =
                    (self.inspector_width - resize.drag_delta().x).clamp(280.0, maximum_width);
            }
            if resize.hovered() || resize.dragged() {
                root.ctx()
                    .set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }

            let divider_x = remaining.max.x - self.inspector_width;
            let viewport = egui::Rect::from_min_max(
                remaining.min,
                egui::pos2(divider_x - 1.0, remaining.max.y),
            );
            let inspector =
                egui::Rect::from_min_max(egui::pos2(divider_x, remaining.min.y), remaining.max);
            self.inspector_at(root, inspector, asset, hdr_supported, status);
            self.viewport_at(root, viewport, reference_available, status)
        } else {
            self.viewport_at(root, remaining, reference_available, status)
        };
        if let Some([radius, thickness, albedo]) = self.baked_atmosphere {
            self.controls.planet_radius_km = radius;
            self.controls.atmosphere_thickness_km = thickness;
            self.controls.ground_albedo_spectral = [albedo; 4];
        }
        self.controls = self.controls.normalized();

        WorkbenchFrame {
            viewport_points,
            actions,
        }
    }

    fn top_toolbar(
        &mut self,
        root: &mut egui::Ui,
        asset: &RealtimeAsset,
        reference_available: bool,
        hdr_supported: bool,
        actions: &mut Vec<WorkbenchAction>,
    ) {
        egui::Panel::top("workbench-toolbar")
            .exact_size(44.0)
            .frame(
                egui::Frame::default()
                    .fill(TOOLBAR_FILL)
                    .inner_margin(egui::Margin::symmetric(10, 7))
                    .stroke(egui::Stroke::new(1.0, BORDER)),
            )
            .show(root, |ui| {
                ui.horizontal_centered(|ui| {
                    match self.mode {
                        WorkbenchMode::Weather => {
                            ui.label(RichText::new("天气可视化工作台").strong().color(ACCENT));
                            ui.label(RichText::new(self.weather_identity()).small().color(MUTED));
                            ui.separator();
                            let paused = self.controls.weather_paused;
                            if ui
                                .add(
                                    egui::Button::new(if paused { "继续" } else { "暂停" })
                                        .selected(paused),
                                )
                                .on_hover_text("暂停或继续天气时钟（空格）")
                                .clicked()
                            {
                                self.controls.weather_paused = !paused;
                            }
                            if ui
                                .button("重置天气")
                                .on_hover_text("恢复行为天气的默认配置")
                                .clicked()
                            {
                                self.controls.reset_weather();
                            }
                            ui.separator();
                        }
                        WorkbenchMode::Reference => {
                            if ui
                                .button("Open...")
                                .on_hover_text("Open an asset.json (Ctrl+O)")
                                .clicked()
                            {
                                actions.push(WorkbenchAction::OpenAsset);
                            }
                            if ui
                                .add_enabled(
                                    self.step_asset(-1).is_some(),
                                    egui::Button::new("Prev"),
                                )
                                .on_hover_text("Previous visible asset (Page Up)")
                                .clicked()
                                && let Some(path) = self.step_asset(-1)
                            {
                                actions.push(WorkbenchAction::LoadAsset(path));
                            }
                            if ui
                                .add_enabled(
                                    self.step_asset(1).is_some(),
                                    egui::Button::new("Next"),
                                )
                                .on_hover_text("Next visible asset (Page Down)")
                                .clicked()
                                && let Some(path) = self.step_asset(1)
                            {
                                actions.push(WorkbenchAction::LoadAsset(path));
                            }

                            ui.separator();
                            for (mode, label, shortcut) in [
                                (CompareMode::Realtime, if self.baked_atmosphere.is_some() { "Baked LUT" } else { "Realtime" }, "1"),
                                (CompareMode::Reference, "Reference", "2"),
                                (CompareMode::AbsoluteDifference, "Absolute Diff", "3"),
                                (CompareMode::SignedDifference, "Signed Diff", "4"),
                            ] {
                                let enabled = mode == CompareMode::Realtime || reference_available;
                                let selected = self.controls.compare_mode == mode;
                                let response = ui.add_enabled(
                                    enabled,
                                    egui::Button::new(label).selected(selected),
                                );
                                let response = if enabled {
                                    response
                                        .on_hover_text(format!("Comparison mode ({shortcut})"))
                                } else {
                                    response.on_disabled_hover_text(
                                        "Reference EXR/transport version unavailable, or sun/observer differs. Re-render legacy PT assets; reset Sun & Observer to match the pose.",
                                    )
                                };
                                if response.clicked() {
                                    self.controls.compare_mode = mode;
                                }
                            }

                            ui.separator();
                            if ui
                                .button("Reset All")
                                .on_hover_text("Restore renderer defaults")
                                .clicked()
                            {
                                self.controls.reset_all(asset);
                                self.ground_albedo_linked = true;
                            }
                            ui.separator();
                        }
                    }

                    let hdr = ui.add_enabled(
                        hdr_supported,
                        egui::Button::new("HDR").selected(self.controls.hdr_enabled),
                    );
                    let hdr = if hdr_supported {
                        hdr.on_hover_text(if self.mode == WorkbenchMode::Weather {
                            "切换 16 位浮点 scRGB 输出（F6）"
                        } else {
                            "Toggle 16-bit floating-point scRGB output (F6)"
                        })
                    } else {
                        hdr.on_disabled_hover_text(if self.mode == WorkbenchMode::Weather {
                            "当前适配器/表面不提供 scRGB Rgba16Float"
                        } else {
                            "The active adapter/surface does not expose scRGB Rgba16Float"
                        })
                    };
                    if hdr.clicked() {
                        self.controls.hdr_enabled = !self.controls.hdr_enabled;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.mode != WorkbenchMode::Weather
                            && ui.selectable_label(self.assets_open, "Assets").clicked()
                        {
                            self.assets_open = !self.assets_open;
                        }
                        if ui
                            .selectable_label(
                                self.inspector_open,
                                if self.mode == WorkbenchMode::Weather {
                                    "设置面板"
                                } else {
                                    "Inspector"
                                },
                            )
                            .clicked()
                        {
                            self.inspector_open = !self.inspector_open;
                        }
                    });
                });
            });
    }

    fn status_bar(
        &self,
        root: &mut egui::Ui,
        asset: &RealtimeAsset,
        reference_available: bool,
        hdr_supported: bool,
        hdr_active: bool,
        status: &WorkbenchStatus,
    ) {
        egui::Panel::bottom("workbench-status")
            .exact_size(28.0)
            .frame(
                egui::Frame::default()
                    .fill(TOOLBAR_FILL)
                    .inner_margin(egui::Margin::symmetric(10, 5))
                    .stroke(egui::Stroke::new(1.0, BORDER)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    if self.mode == WorkbenchMode::Weather {
                        self.weather_status_bar(ui, hdr_supported, hdr_active, status);
                        return;
                    }
                    let manifest = asset.manifest();
                    ui.label(
                        RichText::new(format!(
                            "{} × {}  ·  {} bands  ·  {} spp",
                            manifest.dimensions[0],
                            manifest.dimensions[1],
                            manifest.band_centers_nm.len(),
                            manifest.spp
                        ))
                        .color(MUTED),
                    );
                    ui.separator();
                    ui.label(
                        RichText::new(if hdr_active {
                            "HDR scRGB active"
                        } else if hdr_supported {
                            "SDR output"
                        } else {
                            "SDR output · HDR unavailable"
                        })
                        .color(if hdr_active { ACCENT } else { MUTED }),
                    );
                    ui.separator();
                    ui.label(
                        RichText::new(if reference_available {
                            "Reference ready"
                        } else {
                            "Reference missing"
                        })
                        .color(if reference_available {
                            ACCENT
                        } else {
                            ERROR
                        }),
                    );
                    if self.scanning {
                        ui.separator();
                        ui.spinner();
                        ui.label(RichText::new("Scanning assets").color(MUTED));
                    }
                    if let Some((kind, message)) = &self.status {
                        ui.separator();
                        ui.label(RichText::new(message).color(if *kind == StatusKind::Error {
                            ERROR
                        } else {
                            MUTED
                        }));
                    }
                });
            });
    }

    /// Single-line, self-truncating weather diagnostics strip. Painted
    /// directly (rather than through nested layouts) so the compact 1280-wide
    /// window degrades without wrapping, clipping or invisible text: the state
    /// text receives whatever width the fixed right-hand badges leave and
    /// elides mid-string when it still does not fit.
    fn weather_status_bar(
        &self,
        ui: &mut egui::Ui,
        hdr_supported: bool,
        hdr_active: bool,
        status: &WorkbenchStatus,
    ) {
        let rect = ui.max_rect();
        let painter = ui.painter();
        let font = egui::FontId::proportional(10.0);

        let fps_text = format!(
            "帧率 {:.0} fps{}  ·  {}",
            status.fps,
            if status.capture { "（采集）" } else { "" },
            status.experiment_name
        );
        let fps_galley = painter.layout_no_wrap(fps_text, font.clone(), ACCENT);
        let (hdr_text, hdr_color) = if hdr_active {
            ("HDR scRGB", ACCENT)
        } else if hdr_supported {
            ("SDR", MUTED)
        } else {
            ("SDR · HDR 不可用", MUTED)
        };
        let hdr_galley = painter.layout_no_wrap(hdr_text.to_owned(), font.clone(), hdr_color);

        let gap = 12.0;
        let fps_pos = egui::pos2(
            rect.right() - fps_galley.size().x,
            rect.center().y - fps_galley.size().y / 2.0,
        );
        let hdr_pos = egui::pos2(
            fps_pos.x - gap - hdr_galley.size().x,
            rect.center().y - hdr_galley.size().y / 2.0,
        );
        let available = (hdr_pos.x - gap - rect.left()).max(0.0);
        let state_text = weather_status_line(status, available);
        let mut job = egui::text::LayoutJob::default();
        job.wrap = egui::text::TextWrapping::truncate_at_width(available);
        job.append(&state_text, 0.0, egui::TextFormat::simple(font, MUTED));
        let state_galley = painter.layout_job(job);
        painter.galley(
            egui::pos2(rect.left(), rect.center().y - state_galley.size().y / 2.0),
            state_galley,
            MUTED,
        );
        painter.galley(fps_pos, fps_galley, ACCENT);
        painter.galley(hdr_pos, hdr_galley, hdr_color);
    }

    /// Compact description of the active weather configuration, used by the
    /// toolbar so the fixed kind is visible without opening the inspector.
    fn weather_identity(&self) -> String {
        match self.controls.weather_mode {
            WeatherModeControl::Fixed => format!(
                "{}  ·  {}",
                self.controls.weather_mode.label_zh(),
                weather_kind_label(self.controls.weather_kind)
            ),
            mode => mode.label_zh().to_owned(),
        }
    }

    fn assets_panel(&mut self, root: &mut egui::Ui, actions: &mut Vec<WorkbenchAction>) {
        egui::Panel::left("workbench-assets")
            .resizable(true)
            .default_size(260.0)
            .size_range(190.0..=440.0)
            .frame(
                egui::Frame::default()
                    .fill(PANEL_FILL)
                    .inner_margin(egui::Margin::same(10))
                    .stroke(egui::Stroke::new(1.0, BORDER)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Assets");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("Refresh")
                            .on_hover_text("Rescan root (F5)")
                            .clicked()
                        {
                            actions.push(WorkbenchAction::RefreshCatalog);
                        }
                        if ui
                            .small_button("Folder...")
                            .on_hover_text("Choose scan root")
                            .clicked()
                        {
                            actions.push(WorkbenchAction::ChooseAssetRoot);
                        }
                    });
                });
                ui.label(
                    RichText::new(display_path(&self.asset_root))
                        .small()
                        .color(MUTED),
                )
                .on_hover_text(display_path(&self.asset_root));
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("Filter path or type...")
                        .desired_width(f32::INFINITY),
                );
                ui.separator();

                let visible = self.matching_indices();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if visible.is_empty() {
                            ui.label(RichText::new("No matching asset.json files.").color(MUTED));
                        }
                        for index in visible {
                            let entry = &self.entries[index];
                            let selected = normalized_path(&entry.path) == self.current_path;
                            let label = entry.relative_path.to_string_lossy();
                            let response = ui.selectable_label(
                                selected,
                                RichText::new(label.as_ref()).color(if entry.is_valid() {
                                    Color32::from_rgb(220, 230, 235)
                                } else {
                                    ERROR
                                }),
                            );
                            let response = if let Some(error) = &entry.error {
                                response.on_hover_text(error)
                            } else {
                                response.on_hover_text(display_path(&entry.path))
                            };
                            if response.clicked() && entry.is_valid() && !selected {
                                actions.push(WorkbenchAction::LoadAsset(entry.path.clone()));
                            }
                            if let Some(metadata) = &entry.metadata {
                                ui.label(
                                    RichText::new(format!(
                                        "{}  ·  {}×{}  ·  {} bands  ·  {} spp  ·  sun {:.1}°{}",
                                        metadata.kind,
                                        metadata.dimensions[0],
                                        metadata.dimensions[1],
                                        metadata.bands,
                                        metadata.spp,
                                        metadata.sun_elevation_deg,
                                        if metadata.reference_available {
                                            ""
                                        } else {
                                            "  ·  no EXR"
                                        }
                                    ))
                                    .small()
                                    .color(MUTED),
                                );
                            }
                            ui.add_space(6.0);
                        }
                    });
            });
    }

    fn inspector_at(
        &mut self,
        root: &mut egui::Ui,
        rect: egui::Rect,
        asset: &RealtimeAsset,
        hdr_supported: bool,
        status: &WorkbenchStatus,
    ) {
        root.painter().rect_filled(rect, 0.0, PANEL_FILL);
        root.painter().line_segment(
            [rect.left_top(), rect.left_bottom()],
            egui::Stroke::new(1.0, BORDER),
        );
        let content_rect = rect.shrink(12.0);
        root.scope_builder(
            egui::UiBuilder::new()
                .id_salt("workbench-inspector-content")
                .max_rect(content_rect),
            |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.heading(if self.mode == WorkbenchMode::Weather {
                            "天气设置"
                        } else {
                            "Inspector"
                        });
                        if self.mode == WorkbenchMode::Weather {
                            ui.label(
                                RichText::new("实时天空 · 体积云 · 天气效果")
                                    .small()
                                    .color(MUTED),
                            );
                        }
                        ui.add_space(8.0);

                        if self.mode == WorkbenchMode::Weather {
                            self.weather_inspector(ui, asset, hdr_supported, status);
                            return;
                        }

                        if section_header(ui, "View") {
                            self.controls.reset_view();
                        }
                        resettable_slider(
                            ui,
                            &mut self.controls.view.yaw_deg,
                            0.0..=360.0,
                            "Yaw",
                            ViewState::default().yaw_deg,
                            "Horizontal view direction. Drag the viewport with the left mouse button.",
                        );
                        resettable_slider(
                            ui,
                            &mut self.controls.view.pitch_deg,
                            ViewState::MIN_PITCH_DEG..=ViewState::MAX_PITCH_DEG,
                            "Pitch",
                            ViewState::default().pitch_deg,
                            "Vertical view direction.",
                        );
                        resettable_slider(
                            ui,
                            &mut self.controls.view.fov_y_deg,
                            ViewState::MIN_FOV_Y_DEG..=ViewState::MAX_FOV_Y_DEG,
                            "Vertical FOV",
                            ViewState::default().fov_y_deg,
                            "Vertical field of view. Scroll over the viewport to zoom.",
                        );

                        ui.add_space(10.0);
                        if section_header(ui, "Sun & Observer") {
                            self.controls.reset_sun_observer(asset);
                        }
                        if self.baked_atmosphere.is_some() {
                            ui.label("Baked LUT / wgpu f32. Reset this section to match the selected path-traced reference.");
                            if asset.manifest().transport_version.as_deref()!=Some(sky_assets::asset::LAYERED_TRANSPORT_VERSION) {
                                ui.colored_label(egui::Color32::YELLOW,"Legacy PT reference: re-render with the corrected grazing-layer transport before comparing.");
                            }
                        }
                        resettable_slider(
                            ui,
                            &mut self.controls.sun_azimuth_deg,
                            0.0..=360.0,
                            "Azimuth",
                            asset.manifest().sun_azimuth_deg.rem_euclid(360.0),
                            "Sun direction around the horizon.",
                        );
                        resettable_slider(
                            ui,
                            &mut self.controls.sun_elevation_deg,
                            -90.0..=90.0,
                            "Elevation",
                            asset.manifest().sun_elevation_deg.clamp(-90.0, 90.0),
                            "Sun height above the horizon. Use [ and ] for one-degree steps.",
                        );
                        let maximum_altitude = self.controls.maximum_observer_altitude_km();
                        resettable_log_slider(
                            ui,
                            &mut self.controls.observer_altitude_km,
                            0.0..=maximum_altitude,
                            "Observer altitude (km)",
                            asset
                                .manifest()
                                .observer_altitude_km
                                .clamp(0.0, maximum_altitude),
                            "Observer altitude above the planet surface.",
                        );

                        if self.weather_controls_visible {
                            ui.add_space(10.0);
                            if section_header(ui, "Clouds & Weather") {
                                self.controls.weather_enabled = true;
                                self.controls.weather_mode = WeatherModeControl::Auto;
                                self.controls.weather_kind = WeatherKind::PartlyCloudy;
                                self.controls.weather_climate = Climate::Temperate;
                                self.controls.weather_intensity = 1.0;
                                self.controls.weather_effects = EffectsQuality::High;
                                self.controls.weather_speed = 60.0;
                                self.controls.weather_paused = false;
                                self.controls.weather_seed = crate::controls::DEFAULT_WEATHER_SEED;
                                self.controls.auto_sun = true;
                                self.controls.cloud_quality = CloudQuality::Balanced;
                            }
                            ui.checkbox(
                                &mut self.controls.weather_enabled,
                                "Volumetric clouds & weather effects",
                            )
                            .on_hover_text(
                                "Render the behaviour-first cloud field and its rain, snow, fog and lightning effects.",
                            );
                            ui.add_enabled_ui(self.controls.weather_enabled, |ui| {
                                egui::ComboBox::from_id_salt("weather-mode")
                                    .selected_text(self.controls.weather_mode.label())
                                    .show_ui(ui, |ui| {
                                        for mode in WeatherModeControl::ALL {
                                            ui.selectable_value(
                                                &mut self.controls.weather_mode,
                                                mode,
                                                mode.label(),
                                            );
                                        }
                                    });
                                ui.add_enabled_ui(
                                    self.controls.weather_mode == WeatherModeControl::Fixed,
                                    |ui| {
                                        egui::ComboBox::from_id_salt("weather-kind")
                                            .selected_text(self.controls.weather_kind.label())
                                            .show_ui(ui, |ui| {
                                                for kind in WeatherKind::ALL {
                                                    ui.selectable_value(
                                                        &mut self.controls.weather_kind,
                                                        kind,
                                                        kind.label(),
                                                    );
                                                }
                                            });
                                    },
                                );
                                egui::ComboBox::from_id_salt("weather-climate")
                                    .selected_text(self.controls.weather_climate.label())
                                    .show_ui(ui, |ui| {
                                        for climate in Climate::ALL {
                                            ui.selectable_value(
                                                &mut self.controls.weather_climate,
                                                climate,
                                                climate.label(),
                                            );
                                        }
                                    });
                                ui.add(
                                    egui::Slider::new(
                                        &mut self.controls.weather_intensity,
                                        0.25..=2.5,
                                    )
                                    .text("Weather intensity"),
                                );
                                ui.add(
                                    egui::Slider::new(
                                        &mut self.controls.weather_speed,
                                        MIN_WEATHER_SPEED..=MAX_WEATHER_SPEED,
                                    )
                                    .logarithmic(true)
                                    .text("Simulation speed"),
                                )
                                .on_hover_text("Simulated seconds per wall-clock second.");
                                ui.checkbox(&mut self.controls.weather_paused, "Pause weather");
                                ui.horizontal(|ui| {
                                    ui.label("Seed");
                                    ui.add(egui::DragValue::new(&mut self.controls.weather_seed));
                                });
                                ui.checkbox(&mut self.controls.auto_sun, "Auto sun (diurnal cycle)")
                                    .on_hover_text(
                                        "Drive sun position from the weather model's latitude/day/time. \
                                         Disable to use the Sun & Observer sliders.",
                                    );
                                egui::ComboBox::from_id_salt("weather-fx-quality")
                                    .selected_text(self.controls.weather_effects.label())
                                    .show_ui(ui, |ui| {
                                        for quality in EffectsQuality::ALL {
                                            ui.selectable_value(
                                                &mut self.controls.weather_effects,
                                                quality,
                                                quality.label(),
                                            );
                                        }
                                    });
                                egui::ComboBox::from_id_salt("cloud-quality")
                                    .selected_text(self.controls.cloud_quality.label())
                                    .show_ui(ui, |ui| {
                                        for quality in CloudQuality::ALL {
                                            ui.selectable_value(
                                                &mut self.controls.cloud_quality,
                                                quality,
                                                quality.label(),
                                            );
                                        }
                                    });
                                ui.label(
                                    RichText::new(
                                        "Behaviour-first weather: 20 local states, continuous transitions \
                                         and screen-space rain/snow/fog/lightning. It is approximate \
                                         physics, not a forecast.",
                                    )
                                    .small()
                                    .color(MUTED),
                                );
                            });
                        }

                        ui.add_space(10.0);
                        if section_header(ui, "Display") {
                            self.controls.reset_display();
                        }
                        resettable_slider(
                            ui,
                            &mut self.controls.exposure_ev,
                            MIN_EXPOSURE_EV..=MAX_EXPOSURE_EV,
                            "Exposure compensation (EV)",
                            0.0,
                            "Relative to the calibrated 0.10 display exposure; each EV doubles or halves it.",
                        );
                        resettable_log_slider(
                            ui,
                            &mut self.controls.difference_scale,
                            0.25..=32.0,
                            "Difference scale",
                            4.0,
                            "Magnification applied in absolute and signed difference modes.",
                        );
                        ui.checkbox(&mut self.controls.tone_mapping_enabled, "Tone mapping")
                            .on_hover_text(
                                "Apply the drt-bench Enhanced Reinhard curve and Oklab gamut mapping.",
                            );
                        let hdr = ui.add_enabled(
                            hdr_supported,
                            egui::Checkbox::new(&mut self.controls.hdr_enabled, "HDR scRGB output"),
                        );
                        if hdr_supported {
                            hdr.on_hover_text(
                                "Use a 16-bit floating-point scRGB surface. Enable HDR in Windows for physical HDR output.",
                            );
                        } else {
                            hdr.on_disabled_hover_text(
                                "The active adapter/surface does not expose scRGB Rgba16Float",
                            );
                        }
                        ui.add_enabled_ui(self.controls.tone_mapping_enabled, |ui| {
                            resettable_slider(
                                ui,
                                &mut self.controls.reinhard_overexposure,
                                MIN_REINHARD_OVEREXPOSURE..=MAX_REINHARD_OVEREXPOSURE,
                                "Reinhard asymptote",
                                DEFAULT_REINHARD_OVEREXPOSURE,
                                "Enhanced Reinhard overexposure asymptote. Middle gray remains fixed at 18%.",
                            );
                        });
                        ui.add_enabled_ui(self.controls.hdr_enabled && hdr_supported, |ui| {
                            resettable_slider(
                                ui,
                                &mut self.controls.hdr_paper_white_nits,
                                MIN_HDR_PAPER_WHITE_NITS..=MAX_HDR_PAPER_WHITE_NITS,
                                "HDR paper white (nits)",
                                DEFAULT_HDR_PAPER_WHITE_NITS,
                                "Luminance assigned to mapped display white in the scRGB surface.",
                            );
                            resettable_slider(
                                ui,
                                &mut self.controls.hdr_peak_nits,
                                MIN_HDR_PEAK_NITS..=MAX_HDR_PEAK_NITS,
                                "HDR peak clamp (nits)",
                                DEFAULT_HDR_PEAK_NITS,
                                "Safety clamp for untone-mapped HDR highlights.",
                            );
                        });
                        ui.label(
                            RichText::new(if !self.controls.tone_mapping_enabled {
                                if self.controls.hdr_enabled {
                                    "Output: raw exposed linear scRGB"
                                } else {
                                    "Output: raw exposed linear sRGB (clipped)"
                                }
                            } else if self.controls.hdr_enabled {
                                "Output: linear scRGB / Enhanced Reinhard gamut"
                            } else {
                                "Output: sRGB / Enhanced Reinhard gamut"
                            })
                                .small()
                                .color(MUTED),
                        );

                        ui.add_space(10.0);
                        if self.weather_controls_visible {
                            ui.label("Weather experiment: the balanced atmosphere is solved once; the sun follows the weather clock when Auto sun is on.");
                            ui.label("Cloud diagnostics are shown in the window title once per second.");
                        } else if self.hybrid_atmosphere {
                            ui.label("4D multiple scattering is solved at startup. Sun / altitude updates only rebuild SkyView.");
                            ui.label("Tabulated Earth profile and measured phases; four wavelengths: 450, 510, 580, 650 nm.");
                            if section_header(ui, "Medium") { self.hybrid_draft = [1.0, 0.18]; }
                            ui.add(egui::Slider::new(&mut self.hybrid_draft[0], 0.0..=4.0).text("Aerosol density scale"));
                            ui.add(egui::Slider::new(&mut self.hybrid_draft[1], 0.0..=1.0).text("Ground albedo"));
                            if ui.button("Apply medium / rebuild 4D LUT").clicked() {
                                self.controls.aerosol_turbidity = self.hybrid_draft[0];
                                self.controls.ground_albedo_spectral = [self.hybrid_draft[1]; 4];
                            }
                        } else {
                            ui.label("Atmosphere and ground albedo belong to the baked reference. Re-bake to change them.");
                        }
                    });
            },
        );
    }

    /// Weather-first inspector: behaviour inputs first, then solar/site,
    /// camera and display. Kept separate from the reference/PT layout so the
    /// real-time workbench does not inherit reference-comparison clutter.
    fn weather_inspector(
        &mut self,
        ui: &mut egui::Ui,
        asset: &RealtimeAsset,
        hdr_supported: bool,
        status: &WorkbenchStatus,
    ) {
        let defaults = RealtimeControls::from_asset(asset);
        // 天气控件较多、面板又窄（280–480 pt），这里用更紧凑的行距与滑杆宽度。
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.spacing_mut().slider_width = 104.0;

        if section_header_zh(ui, "天气与云") {
            self.controls.reset_weather();
        }
        ui.checkbox(&mut self.controls.weather_enabled, "体积云与天气效果")
            .on_hover_text("渲染行为优先的云场，以及雨、雪、雾和闪电效果。");
        ui.add_enabled_ui(self.controls.weather_enabled, |ui| {
            ui.add_space(2.0);
            ui.label(RichText::new("快捷预设").small().color(MUTED));
            weather_presets(ui, &mut self.controls);
            ui.add_space(2.0);

            let fixed = self.controls.weather_mode == WeatherModeControl::Fixed;
            labeled_combo(
                ui,
                "模式",
                "weather-mode",
                self.controls.weather_mode.label_zh(),
                |ui| {
                    for mode in WeatherModeControl::ALL {
                        ui.selectable_value(&mut self.controls.weather_mode, mode, mode.label_zh());
                    }
                },
            );
            let kind_response = ui
                .add_enabled_ui(fixed, |ui| {
                    labeled_combo(
                        ui,
                        "天气",
                        "weather-kind",
                        weather_kind_label(self.controls.weather_kind),
                        |ui| {
                            for kind in WeatherKind::ALL {
                                ui.selectable_value(
                                    &mut self.controls.weather_kind,
                                    kind,
                                    weather_kind_label(kind),
                                );
                            }
                        },
                    )
                })
                .inner;
            if !fixed {
                kind_response
                    .on_disabled_hover_text("把“模式”切到「固定天气」，即可锁定 20 种天气之一。");
            }
            labeled_combo(
                ui,
                "气候",
                "weather-climate",
                climate_label(self.controls.weather_climate),
                |ui| {
                    for climate in Climate::ALL {
                        ui.selectable_value(
                            &mut self.controls.weather_climate,
                            climate,
                            climate_label(climate),
                        );
                    }
                },
            );
            ui.add_space(2.0);
            resettable_slider_zh(
                ui,
                &mut self.controls.weather_intensity,
                0.25..=2.5,
                "强度",
                1.0,
                "缩放当前天气行为的强度。",
            );
            resettable_log_slider_zh(
                ui,
                &mut self.controls.weather_speed,
                MIN_WEATHER_SPEED..=MAX_WEATHER_SPEED,
                "模拟速度",
                defaults.weather_speed,
                "每墙钟秒推进的模拟秒数。",
            );
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.controls.weather_paused, "暂停天气");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::DragValue::new(&mut self.controls.weather_seed))
                        .on_hover_text("确定性种子；改变它会重掷局地演变。");
                    ui.label(RichText::new("种子").small().color(MUTED));
                });
            });
            ui.checkbox(&mut self.controls.auto_sun, "自动太阳（昼夜循环）")
                .on_hover_text(
                    "由天气时钟的季节/纬度/年积日/起始时间驱动太阳；\
                     关闭后使用下面的手动太阳与观察者滑杆。",
                );
            labeled_combo(
                ui,
                "云质量",
                "cloud-quality",
                self.controls.cloud_quality.label_zh(),
                |ui| {
                    for quality in CloudQuality::ALL {
                        ui.selectable_value(
                            &mut self.controls.cloud_quality,
                            quality,
                            quality.label(),
                        );
                    }
                },
            );
            labeled_combo(
                ui,
                "效果质量",
                "weather-fx-quality",
                effects_quality_label(self.controls.weather_effects),
                |ui| {
                    for quality in EffectsQuality::ALL {
                        ui.selectable_value(
                            &mut self.controls.weather_effects,
                            quality,
                            effects_quality_label(quality),
                        );
                    }
                },
            );
            ui.add_space(2.0);
            resettable_slider_zh(
                ui,
                &mut self.controls.cloud_occlusion,
                0.0..=1.0,
                "云层遮光强度",
                1.0,
                "缩放天气驱动的云层遮光：按云量/含水量压暗天空、地面与云底。\
                 这是应用层近似，不是辐射输运或真实云影场。",
            );
            ui.add_space(2.0);
            ui.label(
                RichText::new("20 种局地天气状态、连续过渡；近似物理，不是预报。")
                    .small()
                    .color(MUTED),
            );
        });

        ui.add_space(10.0);
        if section_header_zh(ui, "季节与站点") {
            self.controls.weather_season = defaults.weather_season;
            self.controls.weather_latitude_deg = defaults.weather_latitude_deg;
            self.controls.weather_day_of_year = defaults.weather_day_of_year;
            self.controls.weather_start_hour_utc = defaults.weather_start_hour_utc;
        }
        resettable_slider_zh(
            ui,
            &mut self.controls.weather_season,
            0.0..=1.0,
            "季节",
            defaults.weather_season,
            "行为模型的季节输入（0 冬、0.5 春/秋、1 夏），不是观测季节。",
        );
        ui.label(
            RichText::new("0 = 冬季  ·  0.5 = 春/秋  ·  1 = 夏季")
                .small()
                .color(MUTED),
        );
        resettable_slider_zh(
            ui,
            &mut self.controls.weather_latitude_deg,
            -90.0..=90.0,
            "纬度（°）",
            defaults.weather_latitude_deg,
            "天气时钟太阳位置使用的纬度（Michalsky 1988）。",
        );
        resettable_slider_zh(
            ui,
            &mut self.controls.weather_day_of_year,
            1.0..=366.0,
            "年积日",
            defaults.weather_day_of_year,
            "天气时钟太阳位置使用的年积日。",
        );
        resettable_slider_zh(
            ui,
            &mut self.controls.weather_start_hour_utc,
            0.0..=24.0,
            "起始 UTC 小时",
            defaults.weather_start_hour_utc,
            "地方太阳时的起始小时；天气时钟从此处推进。",
        );
        if self.controls.auto_sun {
            ui.label(
                RichText::new("自动太阳：以上季节/站点输入决定太阳位置。")
                    .small()
                    .color(MUTED),
            );
        }

        ui.add_space(10.0);
        if section_header_zh(ui, "太阳与观察者") {
            self.controls.auto_sun = false;
            self.controls.sun_azimuth_deg = defaults.sun_azimuth_deg;
            self.controls.sun_elevation_deg = defaults.sun_elevation_deg;
            self.controls.observer_altitude_km = defaults.observer_altitude_km;
        }
        if self.controls.auto_sun {
            let (azimuth, elevation) = status.weather.as_ref().map_or(
                (
                    self.controls.sun_azimuth_deg,
                    self.controls.sun_elevation_deg,
                ),
                |weather| (weather.sun_azimuth_deg, weather.sun_elevation_deg),
            );
            ui.label(
                RichText::new(format!(
                    "自动太阳 · 方位 {azimuth:.1}° / 高度 {elevation:.1}°"
                ))
                .small()
                .color(ACCENT),
            );
            ui.label(
                RichText::new("关闭「自动太阳」后可手动编辑方位/高度。")
                    .small()
                    .color(MUTED),
            );
        }
        ui.add_enabled_ui(!self.controls.auto_sun, |ui| {
            resettable_slider_zh(
                ui,
                &mut self.controls.sun_azimuth_deg,
                0.0..=360.0,
                "方位",
                defaults.sun_azimuth_deg,
                "太阳绕地平的方位（手动模式）。",
            );
            resettable_slider_zh(
                ui,
                &mut self.controls.sun_elevation_deg,
                -90.0..=90.0,
                "高度",
                defaults.sun_elevation_deg,
                "太阳在地平线以上的高度（手动模式）。",
            );
        });
        let maximum_altitude = self.controls.maximum_observer_altitude_km();
        resettable_log_slider_zh(
            ui,
            &mut self.controls.observer_altitude_km,
            0.0..=maximum_altitude,
            "观察海拔（km）",
            defaults.observer_altitude_km,
            "观察者在星球表面以上的海拔。",
        );

        ui.add_space(10.0);
        if section_header_zh(ui, "相机") {
            self.controls.reset_view();
        }
        resettable_slider_zh(
            ui,
            &mut self.controls.view.yaw_deg,
            0.0..=360.0,
            "偏航",
            ViewState::default().yaw_deg,
            "水平视线方向；也可在视口内按住左键拖动。",
        );
        resettable_slider_zh(
            ui,
            &mut self.controls.view.pitch_deg,
            ViewState::MIN_PITCH_DEG..=ViewState::MAX_PITCH_DEG,
            "俯仰",
            ViewState::default().pitch_deg,
            "垂直视线方向。",
        );
        resettable_slider_zh(
            ui,
            &mut self.controls.view.fov_y_deg,
            ViewState::MIN_FOV_Y_DEG..=ViewState::MAX_FOV_Y_DEG,
            "垂直视场角",
            ViewState::default().fov_y_deg,
            "垂直视场角；在视口上滚动滚轮可缩放。",
        );

        ui.add_space(10.0);
        if section_header_zh(ui, "显示") {
            self.controls.reset_display();
        }
        resettable_slider_zh(
            ui,
            &mut self.controls.exposure_ev,
            MIN_EXPOSURE_EV..=MAX_EXPOSURE_EV,
            "曝光补偿（EV）",
            0.0,
            "相对校准的 0.10 显示曝光；每一档 EV 使曝光翻倍或减半。",
        );
        ui.checkbox(&mut self.controls.tone_mapping_enabled, "色调映射")
            .on_hover_text("应用 Enhanced Reinhard 曲线与 Oklab 色域映射。");
        ui.add_enabled_ui(self.controls.tone_mapping_enabled, |ui| {
            resettable_slider_zh(
                ui,
                &mut self.controls.reinhard_overexposure,
                MIN_REINHARD_OVEREXPOSURE..=MAX_REINHARD_OVEREXPOSURE,
                "Reinhard 渐近线",
                DEFAULT_REINHARD_OVEREXPOSURE,
                "Enhanced Reinhard 的过曝渐近线；中灰保持 18%。",
            );
        });
        let hdr = ui.add_enabled(
            hdr_supported,
            egui::Checkbox::new(&mut self.controls.hdr_enabled, "HDR scRGB 输出"),
        );
        if hdr_supported {
            hdr.on_hover_text(
                "使用 16 位浮点 scRGB 表面；需要在 Windows 中开启 HDR 才能获得物理 HDR 输出。",
            );
        } else {
            hdr.on_disabled_hover_text("当前适配器/表面不提供 scRGB Rgba16Float。");
        }
        ui.add_enabled_ui(self.controls.hdr_enabled && hdr_supported, |ui| {
            resettable_slider_zh(
                ui,
                &mut self.controls.hdr_paper_white_nits,
                MIN_HDR_PAPER_WHITE_NITS..=MAX_HDR_PAPER_WHITE_NITS,
                "HDR 纸白亮度（尼特）",
                DEFAULT_HDR_PAPER_WHITE_NITS,
                "映射到 scRGB 表面显示白色的亮度。",
            );
            resettable_slider_zh(
                ui,
                &mut self.controls.hdr_peak_nits,
                MIN_HDR_PEAK_NITS..=MAX_HDR_PEAK_NITS,
                "HDR 峰值钳制（尼特）",
                DEFAULT_HDR_PEAK_NITS,
                "未做色调映射的高光的峰值钳制。",
            );
        });
        ui.label(
            RichText::new(if !self.controls.tone_mapping_enabled {
                if self.controls.hdr_enabled {
                    "输出：原始曝光的线性 scRGB"
                } else {
                    "输出：原始曝光的线性 sRGB（截断）"
                }
            } else if self.controls.hdr_enabled {
                "输出：线性 scRGB / Enhanced Reinhard 色域"
            } else {
                "输出：sRGB / Enhanced Reinhard 色域"
            })
            .small()
            .color(MUTED),
        );

        ui.add_space(10.0);
        ui.separator();
        ui.label(
            RichText::new(
                "行为优先天气：20 种局地状态、连续过渡，以及屏幕空间雨/雪/雾/闪电。\
                 它是近似物理，不是预报。",
            )
            .small()
            .color(MUTED),
        );
        ui.label(
            RichText::new("管线：sky-realtime → sky-clouds → sky-weather-fx → 显示变换。")
                .small()
                .color(MUTED),
        );
    }

    fn viewport_at(
        &mut self,
        root: &mut egui::Ui,
        viewport: egui::Rect,
        reference_available: bool,
        status: &WorkbenchStatus,
    ) -> egui::Rect {
        let response = root.interact(
            viewport,
            root.make_persistent_id("workbench-viewport"),
            egui::Sense::click_and_drag(),
        );
        if response.dragged_by(egui::PointerButton::Primary) {
            let delta = response.drag_delta();
            self.controls
                .view
                .orbit_pixels(delta.x as f64, delta.y as f64);
        }
        if response.hovered() {
            let scroll = root.input(|input| input.smooth_scroll_delta().y);
            if scroll.abs() > f32::EPSILON {
                self.controls.view.zoom_steps(scroll / 48.0);
            }
        }
        if self.mode == WorkbenchMode::Weather {
            self.weather_hud(root, viewport, status);
            return viewport;
        }
        let hud_rect = egui::Rect::from_min_size(
            viewport.min + egui::vec2(12.0, 12.0),
            egui::vec2(226.0, 56.0),
        );
        root.painter().rect_filled(
            hud_rect,
            5.0,
            Color32::from_rgba_unmultiplied(4, 8, 11, 205),
        );
        root.painter().text(
            hud_rect.min + egui::vec2(10.0, 9.0),
            egui::Align2::LEFT_TOP,
            compare_label(self.controls.compare_mode),
            egui::FontId::proportional(14.0),
            Color32::WHITE,
        );
        root.painter().text(
            hud_rect.min + egui::vec2(10.0, 31.0),
            egui::Align2::LEFT_TOP,
            format!(
                "Sun {:.1}° / {:.1}°  ·  FOV {:.0}°{}",
                self.controls.sun_azimuth_deg,
                self.controls.sun_elevation_deg,
                self.controls.view.fov_y_deg,
                if reference_available {
                    ""
                } else {
                    "  ·  no reference"
                }
            ),
            egui::FontId::proportional(11.0),
            MUTED,
        );
        viewport
    }

    /// Weather-first viewport overlay: current state, solar geometry and
    /// interaction hints. Diagnostics come from the experiment's
    /// `weather_hud`, so this stays a display concern. Everything is painted
    /// inside the viewport rect (never over the inspector) and rows truncate
    /// before they can leave the translucent panel.
    fn weather_hud(&self, root: &mut egui::Ui, viewport: egui::Rect, status: &WorkbenchStatus) {
        let painter = root.painter();
        let rows = weather_hud_rows(status.weather.as_ref(), self.controls.weather_paused);
        let padding = egui::vec2(12.0, 9.0);
        let available_width = (viewport.width() - 24.0).max(40.0);
        let panel_width = available_width.min(470.0);
        let inner_width = (panel_width - padding.x * 2.0).max(40.0);

        let mut galleys = Vec::with_capacity(rows.len());
        let mut content = egui::vec2(0.0, 0.0);
        for (index, row) in rows.iter().enumerate() {
            let font_size = if index == 0 { 15.0 } else { 11.5 };
            let galley = layout_hud_segments(painter, row, font_size, inner_width);
            content.x = content.x.max(galley.size().x);
            content.y += galley.size().y;
            galleys.push(galley);
        }
        let row_gap = 4.0;
        content.y += row_gap * galleys.len().saturating_sub(1) as f32;

        let panel = egui::Rect::from_min_size(
            viewport.min + egui::vec2(12.0, 12.0),
            content + padding * 2.0,
        );
        painter.rect_filled(panel, 6.0, HUD_FILL);
        painter.rect_stroke(
            panel,
            6.0,
            egui::Stroke::new(1.0, HUD_BORDER),
            egui::StrokeKind::Inside,
        );
        let mut y = panel.min.y + padding.y;
        for galley in galleys {
            let row_height = galley.size().y;
            painter.galley(egui::pos2(panel.min.x + padding.x, y), galley, HUD_TEXT);
            y += row_height + row_gap;
        }

        // fps badge, top-right inside the viewport. In a cramped viewport the
        // HUD and the badge cannot share one row, so the badge drops just
        // below the HUD instead of overlapping it.
        let fps = layout_hud_text(
            painter,
            &format!(
                "帧率 {:.0} fps{}  ·  {}",
                status.fps,
                if status.capture { "（采集）" } else { "" },
                status.experiment_name
            ),
            11.0,
            ACCENT,
            available_width,
        );
        let badge_padding = egui::vec2(10.0, 5.0);
        let badge_size = fps.size() + badge_padding * 2.0;
        let mut badge_min = egui::pos2(viewport.max.x - 12.0 - badge_size.x, viewport.min.y + 12.0);
        if badge_min.x < panel.max.x + 12.0 {
            badge_min.y = panel.max.y + 8.0;
        }
        badge_min.y = badge_min
            .y
            .min(viewport.max.y - 12.0 - badge_size.y)
            .max(viewport.min.y + 12.0);
        let badge = egui::Rect::from_min_size(badge_min, badge_size);
        painter.rect_filled(badge, 5.0, HUD_FILL);
        painter.galley(badge.min + badge_padding, fps, ACCENT);

        // Interaction hints, bottom-left inside the viewport. Narrow viewports
        // get the short list instead of a truncated long one.
        let hint_text = if available_width < 480.0 {
            "拖动：视角  ·  滚轮：缩放  ·  空格：暂停  ·  F6：HDR"
        } else {
            "拖动：视角  ·  滚轮：缩放  ·  空格：暂停  ·  [ ]：太阳高度  ·  , .：太阳方位  ·  F6：HDR"
        };
        let hints = layout_hud_text(painter, hint_text, 10.5, HUD_HINT, available_width);
        let hint_padding = egui::vec2(9.0, 4.0);
        let hint = egui::Rect::from_min_size(
            egui::pos2(
                viewport.min.x + 12.0,
                viewport.max.y - 12.0 - hints.size().y - hint_padding.y * 2.0,
            ),
            hints.size() + hint_padding * 2.0,
        );
        painter.rect_filled(hint, 4.0, HUD_HINT_FILL);
        painter.galley(hint.min + hint_padding, hints, HUD_HINT);
    }

    fn filtered_indices(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.is_valid() && entry.matches(&self.filter))
            .map(|(index, _)| index)
            .collect()
    }

    fn matching_indices(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.matches(&self.filter))
            .map(|(index, _)| index)
            .collect()
    }
}

pub fn configure_context(ctx: &egui::Context) {
    install_cjk_font(ctx);
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(9.0, 4.0);
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = PANEL_FILL;
    style.visuals.window_fill = PANEL_FILL;
    style.visuals.extreme_bg_color = Color32::from_rgb(5, 8, 11);
    style.visuals.faint_bg_color = Color32::from_rgb(17, 25, 31);
    style.visuals.selection.bg_fill = ACCENT;
    style.visuals.hyperlink_color = ACCENT;
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(25, 35, 42);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(38, 75, 98);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(48, 112, 154);
    ctx.set_style_of(egui::Theme::Dark, style);
}

/// Install a CJK font so the Chinese workbench UI renders on every platform.
/// egui's bundled fonts contain no CJK glyphs, so the first available system
/// font is appended as a fallback to both text families.
fn install_cjk_font(ctx: &egui::Context) {
    const SYSTEM_FONTS: &[&str] = &[
        r"C:\Windows\Fonts\NotoSansSC-VF.ttf",
        r"C:\Windows\Fonts\Deng.ttf",
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/System/Library/Fonts/PingFang.ttc",
    ];
    let mut candidates: Vec<std::path::PathBuf> =
        SYSTEM_FONTS.iter().map(std::path::PathBuf::from).collect();
    if let Ok(windir) = std::env::var("WINDIR") {
        for name in ["NotoSansSC-VF.ttf", "Deng.ttf", "msyh.ttc", "simhei.ttf"] {
            candidates.push(std::path::Path::new(&windir).join("Fonts").join(name));
        }
    }
    for path in candidates {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "cjk".to_owned(),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push("cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        return;
    }
    eprintln!(
        "warning: 未找到系统中文字体（Noto Sans SC / 等线 / 微软雅黑），界面中文可能显示为方框"
    );
}

/// Weather-workbench section header whose reset button reads 中文.
fn section_header_zh(ui: &mut egui::Ui, title: &str) -> bool {
    section_header_impl(ui, title, "复位")
}

fn section_header(ui: &mut egui::Ui, title: &str) -> bool {
    section_header_impl(ui, title, "Reset")
}

fn section_header_impl(ui: &mut egui::Ui, title: &str, reset_label: &str) -> bool {
    let mut reset = false;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .strong()
                .color(Color32::from_rgb(225, 234, 239)),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            reset = ui.small_button(reset_label).clicked();
        });
    });
    ui.separator();
    reset
}

fn resettable_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    label: &str,
    default: f32,
    tooltip: &str,
) -> bool {
    resettable_slider_impl(
        ui,
        value,
        range,
        label,
        default,
        tooltip,
        "Right-click to reset.",
    )
}

fn resettable_slider_zh(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    label: &str,
    default: f32,
    tooltip: &str,
) -> bool {
    resettable_slider_impl(ui, value, range, label, default, tooltip, "右键复位")
}

fn resettable_slider_impl(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    label: &str,
    default: f32,
    tooltip: &str,
    reset_hint: &str,
) -> bool {
    let response = ui
        .add(
            egui::Slider::new(value, range)
                .text(label)
                .fixed_decimals(2),
        )
        .on_hover_text(format!("{tooltip}\n{reset_hint}"));
    let mut changed = response.changed();
    if response.secondary_clicked() {
        *value = default;
        changed = true;
    }
    changed
}

fn resettable_log_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    label: &str,
    default: f32,
    tooltip: &str,
) -> bool {
    resettable_log_slider_impl(
        ui,
        value,
        range,
        label,
        default,
        tooltip,
        "Right-click to reset.",
    )
}

fn resettable_log_slider_zh(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    label: &str,
    default: f32,
    tooltip: &str,
) -> bool {
    resettable_log_slider_impl(ui, value, range, label, default, tooltip, "右键复位")
}

fn resettable_log_slider_impl(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    label: &str,
    default: f32,
    tooltip: &str,
    reset_hint: &str,
) -> bool {
    let response = ui
        .add(
            egui::Slider::new(value, range)
                .text(label)
                .logarithmic(true)
                .fixed_decimals(2),
        )
        .on_hover_text(format!("{tooltip}\n{reset_hint}"));
    let mut changed = response.changed();
    if response.secondary_clicked() {
        *value = default;
        changed = true;
    }
    changed
}

/// Compact label + combo box row. The combo box takes the rest of the row so
/// every weather selector shares one right edge in the narrow inspector.
fn labeled_combo(
    ui: &mut egui::Ui,
    label: &str,
    id_salt: &str,
    selected_text: &str,
    add_options: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).small().color(MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::ComboBox::from_id_salt(id_salt)
                .selected_text(selected_text)
                .width(ui.available_width())
                .show_ui(ui, add_options)
                .response
        })
        .inner
    })
    .inner
}

/// 一键固定天气预设，让 20 种天气目录在面板里可直接发现；点击即切到固定模式。
fn weather_presets(ui: &mut egui::Ui, controls: &mut RealtimeControls) {
    ui.horizontal_wrapped(|ui| {
        // 比 Inspector 默认间距更紧，使 6 个预设在默认 340 pt 面板里同排显示。
        ui.spacing_mut().item_spacing.x = 4.0;
        for (label, kind, hint) in [
            ("晴空", WeatherKind::Clear, "晴空"),
            ("多云", WeatherKind::PartlyCloudy, "分散积云"),
            ("雨", WeatherKind::Rain, "稳定降雨"),
            ("雷暴", WeatherKind::Thunderstorm, "带闪电的雷暴"),
            ("雪", WeatherKind::LightSnow, "小雪"),
            ("雾", WeatherKind::Fog, "低能见度"),
        ] {
            let selected =
                controls.weather_mode == WeatherModeControl::Fixed && controls.weather_kind == kind;
            if ui
                .selectable_label(selected, RichText::new(label).small())
                .on_hover_text(format!("固定天气预设：{hint}"))
                .clicked()
            {
                controls.weather_mode = WeatherModeControl::Fixed;
                controls.weather_kind = kind;
                controls.weather_paused = false;
            }
        }
    });
}

/// 状态栏的天气单行摘要。窄窗口用紧凑写法，调用方还会再做一次截断，
/// 因此这一行永远不可能溢出。
fn weather_status_line(status: &WorkbenchStatus, available_width: f32) -> String {
    let Some(weather) = &status.weather else {
        return "等待天气诊断…".to_owned();
    };
    let precipitation = if weather.precipitation_rate_mm_h > 0.05 {
        format!(
            "{} {:.2} mm/h",
            weather.precipitation_kind, weather.precipitation_rate_mm_h
        )
    } else {
        "无降水".to_owned()
    };
    if available_width < 880.0 {
        format!(
            "{}  ·  t {:.0} s  ·  气温 {:.1} °C  ·  云量 {:.0} %  ·  能见度 {:.1} km  ·  {precipitation}  ·  风 {:.1} m/s",
            weather.kind,
            weather.sim_time_s,
            weather.temperature_c,
            weather.cloud_cover * 100.0,
            weather.visibility_m / 1_000.0,
            weather.wind_speed_m_s,
        )
    } else {
        format!(
            "{}  ·  t {:.0} s  ·  气温 {:.1} °C  ·  湿度 {:.0} %  ·  云量 {:.0} %  ·  LWP {:.1} g/m²  ·  云底/云顶 {:.0}/{:.0} m  ·  {precipitation}  ·  风 {:.1} m/s ({:.0}°)  ·  能见度 {:.1} km  ·  闪电 {:.1}/min",
            weather.kind,
            weather.sim_time_s,
            weather.temperature_c,
            weather.humidity * 100.0,
            weather.cloud_cover * 100.0,
            weather.lwp_g_m2,
            weather.cloud_base_m,
            weather.cloud_top_m,
            weather.wind_speed_m_s,
            weather.wind_dir_deg,
            weather.visibility_m / 1_000.0,
            weather.lightning_rate_per_min,
        )
    }
}

/// 天气 HUD 的类型化行：每个内层 Vec 是一行带颜色的文本片段。强调色只用于
/// 水汽（降水，蓝色）与危险（低能见度、闪电、浓雾，琥珀色）。
fn weather_hud_rows(hud: Option<&WeatherHud>, paused: bool) -> Vec<Vec<(String, Color32)>> {
    let mut rows: Vec<Vec<(String, Color32)>> = Vec::with_capacity(5);
    let Some(weather) = hud else {
        rows.push(vec![("天气模型启动中…".to_owned(), Color32::WHITE)]);
        rows.push(vec![(
            "行为优先天气：20 种局地状态 + 屏幕空间效果".to_owned(),
            MUTED,
        )]);
        return rows;
    };

    let mut title = vec![(weather.kind.to_owned(), Color32::from_rgb(236, 244, 249))];
    title.push((format!("  ·  t {:.0} s", weather.sim_time_s), MUTED));
    if paused {
        title.push(("  ·  已暂停".to_owned(), WARNING));
    }
    rows.push(title);

    let visibility_low = weather.visibility_m < 5_000.0;
    rows.push(vec![
        (
            format!(
                "气温 {:.1} °C  ·  湿度 {:.0} %  ·  云量 {:.0} %  ·  能见度 ",
                weather.temperature_c,
                weather.humidity * 100.0,
                weather.cloud_cover * 100.0,
            ),
            HUD_TEXT,
        ),
        (
            format!("{:.1} km", weather.visibility_m / 1_000.0),
            if visibility_low { WARNING } else { HUD_TEXT },
        ),
    ]);

    rows.push(vec![(
        format!(
            "太阳方位 {:.1}°  ·  高度 {:.1}°  ·  LWP {:.1} g/m²",
            weather.sun_azimuth_deg, weather.sun_elevation_deg, weather.lwp_g_m2,
        ),
        HUD_TEXT,
    )]);

    let precipitation_active = weather.precipitation_rate_mm_h > 0.05;
    let precipitation = if precipitation_active {
        format!(
            "{} {:.2} mm/h",
            weather.precipitation_kind, weather.precipitation_rate_mm_h
        )
    } else {
        "无降水".to_owned()
    };
    let mut wind = vec![(
        format!(
            "风 {:.1} m/s @ {:.0}°  ·  ",
            weather.wind_speed_m_s, weather.wind_dir_deg,
        ),
        HUD_TEXT,
    )];
    wind.push((
        precipitation,
        if precipitation_active { ACCENT } else { MUTED },
    ));
    if weather.lightning_rate_per_min > 0.05 {
        wind.push((
            format!("  ·  闪电 {:.1}/min", weather.lightning_rate_per_min),
            WARNING,
        ));
    }
    rows.push(wind);

    let mut effects: Vec<(String, Color32)> = Vec::new();
    for (label, value) in [
        ("雾", weather.fog),
        ("沙尘", weather.dust),
        ("彩虹", weather.rainbow),
    ] {
        if value <= 0.05 {
            continue;
        }
        if !effects.is_empty() {
            effects.push(("  ·  ".to_owned(), MUTED));
        }
        effects.push((
            format!("{label} {value:.2}"),
            if value > 0.5 { WARNING } else { MUTED },
        ));
    }
    if !effects.is_empty() {
        rows.push(effects);
    }
    rows
}

fn layout_hud_segments(
    painter: &egui::Painter,
    segments: &[(String, Color32)],
    font_size: f32,
    max_width: f32,
) -> Arc<egui::Galley> {
    let font = egui::FontId::proportional(font_size);
    let mut job = egui::text::LayoutJob::default();
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width);
    for (text, color) in segments {
        job.append(text, 0.0, egui::TextFormat::simple(font.clone(), *color));
    }
    painter.layout_job(job)
}

fn layout_hud_text(
    painter: &egui::Painter,
    text: &str,
    font_size: f32,
    color: Color32,
    max_width: f32,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width);
    job.append(
        text,
        0.0,
        egui::TextFormat::simple(egui::FontId::proportional(font_size), color),
    );
    painter.layout_job(job)
}

fn compare_label(mode: CompareMode) -> &'static str {
    match mode {
        CompareMode::Realtime => "Realtime",
        CompareMode::Reference => "Offline Reference",
        CompareMode::AbsoluteDifference => "Absolute Difference",
        CompareMode::SignedDifference => "Signed Difference",
    }
}

fn normalized_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    })
}

fn display_path(path: &Path) -> String {
    let display = path.display().to_string();
    display.strip_prefix(r"\\?\").unwrap_or(&display).to_owned()
}
