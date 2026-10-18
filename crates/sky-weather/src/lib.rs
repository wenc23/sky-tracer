//! Behaviour-first weather and cloud-field model.
//!
//! This is deliberately **not** a physical weather simulation or a forecast.
//! It produces a wide, coherent catalogue of local weather behaviour --
//! clouds, rain, snow, fog, dust, lightning and wind -- with a fast 2D field
//! and a state director. See `docs/weather.md` for the contract.

use std::error::Error;

use serde::{Deserialize, Serialize};

pub mod director;
pub mod effects;
pub mod kind;
pub mod microphysics;
pub mod model;
pub mod radiation;
pub mod rng;
pub mod solar;
pub mod surface;
pub mod thermo;
pub mod volume;

pub use director::{SystemKind, WeatherDirector, WeatherSystem};
pub use effects::{EffectsState, Precipitation, WeatherFrame};
pub use kind::{
    Climate, CloudProfile, EffectsQuality, PrecipitationKind, Target, WeatherKeyframe, WeatherKind,
    WeatherMode,
};
pub use model::Model;

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
pub type Diagnostics = WeatherFrame;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub mode: WeatherMode,
    pub climate: Climate,
    /// 0 = winter, 0.5 = spring/autumn, 1 = summer.
    pub season: f32,
    pub intensity: f32,
    pub grid: [u32; 2],
    pub dx_m: f64,
    pub dt_s: f64,
    pub seed: u64,
    pub latitude_deg: f64,
    pub longitude_deg: f64,
    pub day_of_year: f64,
    pub start_hour_utc: f64,
    pub auto_sun: bool,
    pub sun_elevation_deg: f64,
    pub sun_azimuth_deg: f64,
    pub volume_dims: [u32; 3],
    pub volume_extent_m: [f64; 3],
    pub effects_quality: EffectsQuality,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: WeatherMode::Auto,
            climate: Climate::Temperate,
            season: 0.55,
            intensity: 1.0,
            grid: [128, 128],
            dx_m: 250.0,
            dt_s: 1.0,
            seed: 0xC10D_2026_1017,
            latitude_deg: 45.0,
            longitude_deg: 0.0,
            day_of_year: 172.0,
            start_hour_utc: 9.0,
            auto_sun: true,
            sun_elevation_deg: 30.0,
            sun_azimuth_deg: 180.0,
            volume_dims: [128, 96, 128],
            volume_extent_m: [32_000.0, 14_000.0, 32_000.0],
            effects_quality: EffectsQuality::High,
        }
    }
}

impl Config {
    pub fn fixed(kind: WeatherKind) -> Self {
        Self {
            mode: WeatherMode::Fixed(kind),
            ..Self::default()
        }
    }

    pub fn auto(climate: Climate) -> Self {
        Self {
            mode: WeatherMode::Auto,
            climate,
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<()> {
        let dims_ok = self.volume_dims.iter().all(|&n| (8..=256).contains(&n));
        if !(16..=512).contains(&self.grid[0])
            || !(16..=512).contains(&self.grid[1])
            || !self.dx_m.is_finite()
            || !(20.0..=10_000.0).contains(&self.dx_m)
            || !self.dt_s.is_finite()
            || !(0.1..=30.0).contains(&self.dt_s)
            || !self.season.is_finite()
            || !(0.0..=1.0).contains(&self.season)
            || !self.intensity.is_finite()
            || !(0.25..=2.5).contains(&self.intensity)
            || !self.latitude_deg.is_finite()
            || !(-90.0..=90.0).contains(&self.latitude_deg)
            || !self.longitude_deg.is_finite()
            || !(-180.0..=180.0).contains(&self.longitude_deg)
            || !self.day_of_year.is_finite()
            || !(1.0..=366.0).contains(&self.day_of_year)
            || !self.start_hour_utc.is_finite()
            || !(0.0..24.0).contains(&self.start_hour_utc)
            || !self.sun_elevation_deg.is_finite()
            || !(-90.0..=90.0).contains(&self.sun_elevation_deg)
            || !self.sun_azimuth_deg.is_finite()
            || !dims_ok
            || self
                .volume_extent_m
                .iter()
                .any(|v| !v.is_finite() || *v <= 0.0)
        {
            return Err("invalid weather configuration".into());
        }
        if let WeatherMode::Timeline(frames) = &self.mode {
            if frames
                .windows(2)
                .any(|pair| pair[1].time_s < pair[0].time_s)
            {
                return Err("weather timeline must be sorted".into());
            }
        }
        Ok(())
    }

    pub fn domain_m(&self) -> f32 {
        (self.grid[0].min(self.grid[1]) as f64 * self.dx_m) as f32
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SunPosition {
    pub elevation_deg: f64,
    pub azimuth_deg: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CloudVolume {
    pub dims: [u32; 3],
    pub origin_m: [f32; 3],
    pub spacing_m: [f32; 3],
    /// RGBA f32, x fastest. Channels: LWC kg/m3, r_eff m, ice fraction,
    /// cloud fraction.
    pub data: Vec<f32>,
}

impl CloudVolume {
    pub fn validate(&self) -> Result<()> {
        let dims = self.dims.map(u64::from);
        let expected = dims[0]
            .checked_mul(dims[1])
            .and_then(|v| v.checked_mul(dims[2]))
            .and_then(|v| v.checked_mul(4))
            .ok_or("cloud volume dimensions overflow")?;
        if self.data.len() as u64 != expected {
            return Err(format!(
                "cloud volume has {} values, expected {expected}",
                self.data.len()
            )
            .into());
        }
        if self.dims.iter().any(|&n| n == 0)
            || self
                .origin_m
                .iter()
                .chain(self.spacing_m.iter())
                .any(|v| !v.is_finite())
            || self.spacing_m.iter().any(|&v| v <= 0.0)
            || self.data.iter().any(|v| !v.is_finite() || *v < 0.0)
        {
            return Err("cloud volume contains invalid geometry or values".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub(crate) config: Config,
    pub(crate) time_s: f64,
    pub(crate) sun_override: Option<[f64; 2]>,
    pub(crate) director: WeatherDirector,
    pub(crate) columns: Vec<model::Column>,
    pub(crate) wind_u: Vec<f32>,
    pub(crate) wind_v: Vec<f32>,
    pub(crate) accumulator_s: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_validates() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn broken_config_is_rejected() {
        let mut config = Config::default();
        config.grid = [4, 4];
        assert!(config.validate().is_err());
        config.grid = [64, 64];
        config.volume_dims = [4, 4, 4];
        assert!(config.validate().is_err());
    }
}
