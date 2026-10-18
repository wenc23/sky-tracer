//! Public weather frame and GPU effect inputs.
//!
//! The frame is a description of *behaviour*, not a meteorological analysis.
//! Values are deliberately rounded and stable enough for HUD display and for
//! driving the procedural effects layer.

use serde::{Deserialize, Serialize};

use crate::{
    SunPosition,
    kind::{PrecipitationKind, WeatherKind},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Precipitation {
    pub kind: PrecipitationKind,
    pub rate_mm_h: f64,
    /// 0..1 visual intensity after the director's intensity scaling.
    pub intensity: f32,
    pub snow_fraction: f32,
    pub hail_fraction: f32,
}

impl Precipitation {
    pub fn is_active(self) -> bool {
        self.kind != PrecipitationKind::None && self.rate_mm_h > 0.01
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WeatherFrame {
    pub time_s: f64,
    pub kind: WeatherKind,
    pub intensity: f32,
    pub temperature_c: f64,
    pub dew_point_c: f64,
    pub humidity: f64,
    pub pressure_hpa: f64,
    pub wind_speed_m_s: f64,
    pub wind_dir_deg: f64,
    pub gust_m_s: f64,
    pub cloud_cover: f64,
    pub lwp_g_m2: f64,
    pub cloud_base_m: f64,
    pub cloud_top_m: f64,
    pub precipitation: Precipitation,
    pub fog_density: f64,
    pub dust_density: f64,
    pub visibility_m: f64,
    pub lightning_rate_per_min: f64,
    pub rainbow_strength: f64,
    pub cloud_darkening: f64,
    pub sun: SunPosition,
}

impl Default for WeatherFrame {
    fn default() -> Self {
        Self {
            time_s: 0.0,
            kind: WeatherKind::Clear,
            intensity: 1.0,
            temperature_c: 15.0,
            dew_point_c: 5.0,
            humidity: 0.5,
            pressure_hpa: 1013.25,
            wind_speed_m_s: 3.0,
            wind_dir_deg: 180.0,
            gust_m_s: 4.0,
            cloud_cover: 0.0,
            lwp_g_m2: 0.0,
            cloud_base_m: 1_000.0,
            cloud_top_m: 1_000.0,
            precipitation: Precipitation::default(),
            fog_density: 0.0,
            dust_density: 0.0,
            visibility_m: 50_000.0,
            lightning_rate_per_min: 0.0,
            rainbow_strength: 0.0,
            cloud_darkening: 0.0,
            sun: SunPosition::default(),
        }
    }
}

/// Inputs consumed by `sky-weather-fx`. All values are already clamped to
/// 0..1 or a documented range by [`EffectsState::sanitize`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EffectsState {
    pub time_s: f32,
    pub rain: f32,
    pub snow: f32,
    pub hail: f32,
    pub sleet: f32,
    pub fog: f32,
    pub dust: f32,
    pub wind: [f32; 2],
    pub cloud_darkening: f32,
    pub rainbow: f32,
    pub lightning_flash: f32,
    pub lightning_seed: u32,
}

impl EffectsState {
    pub fn sanitize(mut self) -> Self {
        self.time_s = finite_or(self.time_s, 0.0);
        self.rain = finite_clamp(self.rain, 0.0, 1.0);
        self.snow = finite_clamp(self.snow, 0.0, 1.0);
        self.hail = finite_clamp(self.hail, 0.0, 1.0);
        self.sleet = finite_clamp(self.sleet, 0.0, 1.0);
        self.fog = finite_clamp(self.fog, 0.0, 1.0);
        self.dust = finite_clamp(self.dust, 0.0, 1.0);
        self.wind[0] = finite_clamp(self.wind[0], -1.0, 1.0);
        self.wind[1] = finite_clamp(self.wind[1], -1.0, 1.0);
        self.cloud_darkening = finite_clamp(self.cloud_darkening, 0.0, 1.0);
        self.rainbow = finite_clamp(self.rainbow, 0.0, 1.0);
        self.lightning_flash = finite_clamp(self.lightning_flash, 0.0, 1.0);
        self
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

fn finite_clamp(value: f32, low: f32, high: f32) -> f32 {
    finite_or(value, low).clamp(low, high)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_clamps_effects() {
        let state = EffectsState {
            rain: -1.0,
            snow: 4.0,
            fog: f32::NAN,
            wind: [f32::INFINITY, -4.0],
            ..EffectsState::default()
        }
        .sanitize();
        assert_eq!(state.rain, 0.0);
        assert_eq!(state.snow, 1.0);
        assert_eq!(state.fog, 0.0);
        assert_eq!(state.wind, [-1.0, -1.0]);
    }

    #[test]
    fn precipitation_activity_requires_rate_and_kind() {
        let mut p = Precipitation {
            kind: PrecipitationKind::Rain,
            rate_mm_h: 0.0,
            ..Precipitation::default()
        };
        assert!(!p.is_active());
        p.rate_mm_h = 1.0;
        assert!(p.is_active());
    }
}
