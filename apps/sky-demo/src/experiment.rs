use crate::assets::RealtimeAsset;
use crate::color::DisplayTransform;
use crate::controls::RealtimeControls;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CompareMode {
    #[default]
    Realtime,
    Reference,
    AbsoluteDifference,
    SignedDifference,
}

impl CompareMode {
    pub const fn shader_id(self) -> f32 {
        match self {
            Self::Realtime => 0.0,
            Self::Reference => 1.0,
            Self::AbsoluteDifference => 2.0,
            Self::SignedDifference => 3.0,
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Realtime => Self::Reference,
            Self::Reference => Self::AbsoluteDifference,
            Self::AbsoluteDifference => Self::SignedDifference,
            Self::SignedDifference => Self::Realtime,
        }
    }
}

pub struct ExperimentInit<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub surface_format: wgpu::TextureFormat,
    pub asset: &'a RealtimeAsset,
    pub display: DisplayTransform,
}

pub struct UpdateContext<'a> {
    pub controls: &'a RealtimeControls,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceViewport {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Compact, display-ready behaviour-weather diagnostics for the workbench HUD.
/// This is a UI summary; the authoritative contract lives in
/// `sky_weather::WeatherFrame`.
#[derive(Clone, Debug, PartialEq)]
pub struct WeatherHud {
    pub kind: &'static str,
    pub temperature_c: f32,
    pub dew_point_c: f32,
    pub humidity: f32,
    pub cloud_cover: f32,
    pub lwp_g_m2: f32,
    pub cloud_base_m: f32,
    pub cloud_top_m: f32,
    pub precipitation_kind: &'static str,
    pub precipitation_rate_mm_h: f32,
    pub wind_speed_m_s: f32,
    pub wind_dir_deg: f32,
    pub visibility_m: f32,
    pub lightning_rate_per_min: f32,
    pub rainbow: f32,
    pub fog: f32,
    pub dust: f32,
    pub sun_elevation_deg: f32,
    pub sun_azimuth_deg: f32,
    pub sim_time_s: f64,
}

pub struct FrameContext<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub target: &'a wgpu::TextureView,
    pub viewport: SurfaceViewport,
}

pub trait RealtimeExperiment {
    fn name(&self) -> &'static str;

    /// Headless photometric validation bypasses exposure and display mapping.
    fn set_linear_output(&mut self, _enabled: bool) {}

    fn update(&mut self, _context: UpdateContext<'_>) {}

    fn reference_available(&self) -> bool {
        false
    }

    /// Optional one-line diagnostic for the window title/status area.
    fn status_line(&self) -> Option<String> {
        None
    }

    /// Optional structured weather diagnostics for the workbench HUD.
    fn weather_hud(&self) -> Option<WeatherHud> {
        None
    }

    /// Apply the workbench startup controls before the first frame. Weather
    /// experiments override this to rebuild their behaviour model immediately
    /// so `warm_up` advances the same model the first frame will present.
    fn configure(&mut self, _controls: &RealtimeControls) -> Result<(), String> {
        Ok(())
    }

    /// Advance simulation state before the first presented frame (used by
    /// headless capture and by the weather workbench startup).
    fn warm_up(&mut self, _sim_seconds: f64) -> Result<(), String> {
        Ok(())
    }

    fn render(&mut self, context: FrameContext<'_>);
}
