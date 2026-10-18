//! Behavior-first weather catalogue.
//!
//! These values are *behaviour targets*, not a physical forecast. They keep
//! cloud, precipitation, visibility and wind mutually plausible while allowing
//! a much wider range of weather than the old physical bulk presets.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectsQuality {
    Low,
    #[default]
    High,
}

impl EffectsQuality {
    pub const ALL: [Self; 2] = [Self::Low, Self::High];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::High => "High",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrecipitationKind {
    #[default]
    None,
    Rain,
    Snow,
    Sleet,
    Hail,
    FreezingRain,
}

impl PrecipitationKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Rain => "rain",
            Self::Snow => "snow",
            Self::Sleet => "sleet",
            Self::Hail => "hail",
            Self::FreezingRain => "freezing rain",
        }
    }

    pub const fn is_frozen(self) -> bool {
        matches!(self, Self::Snow | Self::Sleet | Self::Hail)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloudProfile {
    #[default]
    None,
    Cumulus,
    Stratus,
    Deep,
    Snow,
    Cirrus,
    Fog,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Climate {
    Tropical,
    #[default]
    Temperate,
    Continental,
    Arid,
    Polar,
    Alpine,
}

impl Climate {
    pub const ALL: [Self; 6] = [
        Self::Tropical,
        Self::Temperate,
        Self::Continental,
        Self::Arid,
        Self::Polar,
        Self::Alpine,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Tropical => "Tropical",
            Self::Temperate => "Temperate",
            Self::Continental => "Continental",
            Self::Arid => "Arid",
            Self::Polar => "Polar",
            Self::Alpine => "Alpine",
        }
    }

    /// Mean near-surface temperature in Celsius, before season and weather
    /// offsets. These are behaviour baselines, not climatological normals.
    pub const fn base_temperature_c(self) -> f32 {
        match self {
            Self::Tropical => 27.0,
            Self::Temperate => 15.0,
            Self::Continental => 12.0,
            Self::Arid => 25.0,
            Self::Polar => -5.0,
            Self::Alpine => 5.0,
        }
    }

    pub const fn base_humidity(self) -> f32 {
        match self {
            Self::Tropical => 0.78,
            Self::Temperate => 0.62,
            Self::Continental => 0.55,
            Self::Arid => 0.25,
            Self::Polar => 0.72,
            Self::Alpine => 0.58,
        }
    }

    pub const fn base_wind_m_s(self) -> f32 {
        match self {
            Self::Tropical => 5.0,
            Self::Temperate => 6.0,
            Self::Continental => 7.0,
            Self::Arid => 8.0,
            Self::Polar => 7.0,
            Self::Alpine => 7.5,
        }
    }

    pub const fn supports_snow(self) -> bool {
        !matches!(self, Self::Tropical | Self::Arid)
    }

    pub const fn is_tropical(self) -> bool {
        matches!(self, Self::Tropical)
    }

    pub const fn is_dry(self) -> bool {
        matches!(self, Self::Arid)
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum WeatherKind {
    #[default]
    Clear,
    Fair,
    PartlyCloudy,
    Overcast,
    Fog,
    Haze,
    Dust,
    Drizzle,
    Rain,
    HeavyRain,
    TropicalShower,
    Thunderstorm,
    Hailstorm,
    Supercell,
    LightSnow,
    HeavySnow,
    Blizzard,
    Sleet,
    FreezingRain,
    Gale,
}

impl WeatherKind {
    pub const ALL: [Self; 20] = [
        Self::Clear,
        Self::Fair,
        Self::PartlyCloudy,
        Self::Overcast,
        Self::Fog,
        Self::Haze,
        Self::Dust,
        Self::Drizzle,
        Self::Rain,
        Self::HeavyRain,
        Self::TropicalShower,
        Self::Thunderstorm,
        Self::Hailstorm,
        Self::Supercell,
        Self::LightSnow,
        Self::HeavySnow,
        Self::Blizzard,
        Self::Sleet,
        Self::FreezingRain,
        Self::Gale,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Clear => "Clear",
            Self::Fair => "Fair cumulus",
            Self::PartlyCloudy => "Partly cloudy",
            Self::Overcast => "Overcast",
            Self::Fog => "Fog",
            Self::Haze => "Haze",
            Self::Dust => "Dust storm",
            Self::Drizzle => "Drizzle",
            Self::Rain => "Rain",
            Self::HeavyRain => "Heavy rain",
            Self::TropicalShower => "Tropical shower",
            Self::Thunderstorm => "Thunderstorm",
            Self::Hailstorm => "Hailstorm",
            Self::Supercell => "Supercell",
            Self::LightSnow => "Light snow",
            Self::HeavySnow => "Heavy snow",
            Self::Blizzard => "Blizzard",
            Self::Sleet => "Sleet",
            Self::FreezingRain => "Freezing rain",
            Self::Gale => "Gale",
        }
    }

    pub const fn is_precipitating(self) -> bool {
        !matches!(
            self,
            Self::Clear
                | Self::Fair
                | Self::PartlyCloudy
                | Self::Overcast
                | Self::Haze
                | Self::Gale
        )
    }

    /// A behaviour target. Values are intentionally broad and visible.
    pub const fn target(self) -> Target {
        use CloudProfile::*;
        use PrecipitationKind as P;
        match self {
            Self::Clear => Target {
                temperature_offset_c: 2.0,
                humidity: 0.35,
                cloud_cover: 0.02,
                cloud_base_m: 2_000.0,
                cloud_top_m: 2_050.0,
                cloud_water: 0.0,
                reff_m: 10.0e-6,
                precipitation_mm_h: 0.0,
                precipitation: P::None,
                snow_fraction: 0.0,
                hail_fraction: 0.0,
                fog: 0.0,
                dust: 0.0,
                visibility_m: 40_000.0,
                wind_m_s: 3.0,
                wind_dir_deg: 180.0,
                gust_factor: 1.1,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.0,
                ice_fraction: 0.0,
                profile: None,
            },
            Self::Fair => Target {
                temperature_offset_c: 1.0,
                humidity: 0.52,
                cloud_cover: 0.18,
                cloud_base_m: 1_200.0,
                cloud_top_m: 2_400.0,
                cloud_water: 0.6e-3,
                reff_m: 11.0e-6,
                precipitation_mm_h: 0.0,
                precipitation: P::None,
                snow_fraction: 0.0,
                hail_fraction: 0.0,
                fog: 0.0,
                dust: 0.0,
                visibility_m: 35_000.0,
                wind_m_s: 5.0,
                wind_dir_deg: 160.0,
                gust_factor: 1.2,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.02,
                ice_fraction: 0.0,
                profile: Cumulus,
            },
            Self::PartlyCloudy => Target {
                temperature_offset_c: 0.5,
                humidity: 0.58,
                cloud_cover: 0.45,
                cloud_base_m: 1_000.0,
                cloud_top_m: 2_600.0,
                cloud_water: 1.0e-3,
                reff_m: 12.0e-6,
                precipitation_mm_h: 0.0,
                precipitation: P::None,
                snow_fraction: 0.0,
                hail_fraction: 0.0,
                fog: 0.0,
                dust: 0.0,
                visibility_m: 30_000.0,
                wind_m_s: 6.0,
                wind_dir_deg: 170.0,
                gust_factor: 1.2,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.08,
                ice_fraction: 0.0,
                profile: Cumulus,
            },
            Self::Overcast => Target {
                temperature_offset_c: -1.0,
                humidity: 0.82,
                cloud_cover: 0.95,
                cloud_base_m: 400.0,
                cloud_top_m: 1_400.0,
                cloud_water: 1.2e-3,
                reff_m: 9.0e-6,
                precipitation_mm_h: 0.0,
                precipitation: P::None,
                snow_fraction: 0.0,
                hail_fraction: 0.0,
                fog: 0.0,
                dust: 0.0,
                visibility_m: 20_000.0,
                wind_m_s: 6.0,
                wind_dir_deg: 200.0,
                gust_factor: 1.2,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.22,
                ice_fraction: 0.0,
                profile: Stratus,
            },
            Self::Fog => Target {
                temperature_offset_c: -2.0,
                humidity: 0.99,
                cloud_cover: 0.85,
                cloud_base_m: 40.0,
                cloud_top_m: 450.0,
                cloud_water: 0.4e-3,
                reff_m: 7.0e-6,
                precipitation_mm_h: 0.0,
                precipitation: P::None,
                snow_fraction: 0.0,
                hail_fraction: 0.0,
                fog: 0.9,
                dust: 0.0,
                visibility_m: 250.0,
                wind_m_s: 1.5,
                wind_dir_deg: 210.0,
                gust_factor: 1.0,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.18,
                ice_fraction: 0.0,
                profile: Fog,
            },
            Self::Haze => Target {
                temperature_offset_c: 1.0,
                humidity: 0.55,
                cloud_cover: 0.15,
                cloud_base_m: 1_500.0,
                cloud_top_m: 2_000.0,
                cloud_water: 0.0,
                reff_m: 8.0e-6,
                precipitation_mm_h: 0.0,
                precipitation: P::None,
                snow_fraction: 0.0,
                hail_fraction: 0.0,
                fog: 0.12,
                dust: 0.15,
                visibility_m: 5_000.0,
                wind_m_s: 2.0,
                wind_dir_deg: 150.0,
                gust_factor: 1.0,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.08,
                ice_fraction: 0.0,
                profile: None,
            },
            Self::Dust => Target {
                temperature_offset_c: 4.0,
                humidity: 0.18,
                cloud_cover: 0.05,
                cloud_base_m: 2_500.0,
                cloud_top_m: 2_600.0,
                cloud_water: 0.0,
                reff_m: 8.0e-6,
                precipitation_mm_h: 0.0,
                precipitation: P::None,
                snow_fraction: 0.0,
                hail_fraction: 0.0,
                fog: 0.0,
                dust: 0.75,
                visibility_m: 1_200.0,
                wind_m_s: 14.0,
                wind_dir_deg: 230.0,
                gust_factor: 1.6,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.18,
                ice_fraction: 0.0,
                profile: None,
            },
            Self::Drizzle => Target {
                temperature_offset_c: -0.5,
                humidity: 0.9,
                cloud_cover: 0.85,
                cloud_base_m: 500.0,
                cloud_top_m: 1_300.0,
                cloud_water: 0.9e-3,
                reff_m: 9.0e-6,
                precipitation_mm_h: 0.4,
                precipitation: P::Rain,
                snow_fraction: 0.0,
                hail_fraction: 0.0,
                fog: 0.12,
                dust: 0.0,
                visibility_m: 6_000.0,
                wind_m_s: 5.0,
                wind_dir_deg: 200.0,
                gust_factor: 1.2,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.2,
                ice_fraction: 0.0,
                profile: Stratus,
            },
            Self::Rain => Target {
                temperature_offset_c: -0.5,
                humidity: 0.94,
                cloud_cover: 0.92,
                cloud_base_m: 550.0,
                cloud_top_m: 2_800.0,
                cloud_water: 1.4e-3,
                reff_m: 11.0e-6,
                precipitation_mm_h: 5.0,
                precipitation: P::Rain,
                snow_fraction: 0.0,
                hail_fraction: 0.0,
                fog: 0.05,
                dust: 0.0,
                visibility_m: 5_000.0,
                wind_m_s: 7.0,
                wind_dir_deg: 210.0,
                gust_factor: 1.3,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.3,
                ice_fraction: 0.0,
                profile: Stratus,
            },
            Self::HeavyRain => Target {
                temperature_offset_c: -1.0,
                humidity: 0.98,
                cloud_cover: 0.98,
                cloud_base_m: 600.0,
                cloud_top_m: 5_000.0,
                cloud_water: 2.2e-3,
                reff_m: 13.0e-6,
                precipitation_mm_h: 28.0,
                precipitation: P::Rain,
                snow_fraction: 0.0,
                hail_fraction: 0.0,
                fog: 0.1,
                dust: 0.0,
                visibility_m: 2_000.0,
                wind_m_s: 9.0,
                wind_dir_deg: 220.0,
                gust_factor: 1.5,
                lightning_rate_per_min: 1.0,
                rainbow: 0.0,
                darkening: 0.45,
                ice_fraction: 0.0,
                profile: Deep,
            },
            Self::TropicalShower => Target {
                temperature_offset_c: 1.0,
                humidity: 0.97,
                cloud_cover: 0.8,
                cloud_base_m: 900.0,
                cloud_top_m: 6_000.0,
                cloud_water: 2.0e-3,
                reff_m: 14.0e-6,
                precipitation_mm_h: 22.0,
                precipitation: P::Rain,
                snow_fraction: 0.0,
                hail_fraction: 0.0,
                fog: 0.02,
                dust: 0.0,
                visibility_m: 3_000.0,
                wind_m_s: 6.0,
                wind_dir_deg: 120.0,
                gust_factor: 1.4,
                lightning_rate_per_min: 2.0,
                rainbow: 0.35,
                darkening: 0.32,
                ice_fraction: 0.0,
                profile: Deep,
            },
            Self::Thunderstorm => Target {
                temperature_offset_c: -1.0,
                humidity: 0.96,
                cloud_cover: 0.95,
                cloud_base_m: 450.0,
                cloud_top_m: 9_000.0,
                cloud_water: 2.5e-3,
                reff_m: 15.0e-6,
                precipitation_mm_h: 50.0,
                precipitation: P::Rain,
                snow_fraction: 0.0,
                hail_fraction: 0.0,
                fog: 0.02,
                dust: 0.0,
                visibility_m: 1_500.0,
                wind_m_s: 12.0,
                wind_dir_deg: 240.0,
                gust_factor: 1.9,
                lightning_rate_per_min: 14.0,
                rainbow: 0.0,
                darkening: 0.55,
                ice_fraction: 0.15,
                profile: Deep,
            },
            Self::Hailstorm => Target {
                temperature_offset_c: -2.0,
                humidity: 0.96,
                cloud_cover: 0.98,
                cloud_base_m: 420.0,
                cloud_top_m: 10_500.0,
                cloud_water: 2.8e-3,
                reff_m: 17.0e-6,
                precipitation_mm_h: 70.0,
                precipitation: P::Hail,
                snow_fraction: 0.0,
                hail_fraction: 0.55,
                fog: 0.02,
                dust: 0.0,
                visibility_m: 900.0,
                wind_m_s: 14.0,
                wind_dir_deg: 245.0,
                gust_factor: 2.1,
                lightning_rate_per_min: 22.0,
                rainbow: 0.0,
                darkening: 0.62,
                ice_fraction: 0.35,
                profile: Deep,
            },
            Self::Supercell => Target {
                temperature_offset_c: -2.0,
                humidity: 0.95,
                cloud_cover: 1.0,
                cloud_base_m: 380.0,
                cloud_top_m: 12_000.0,
                cloud_water: 3.2e-3,
                reff_m: 18.0e-6,
                precipitation_mm_h: 90.0,
                precipitation: P::Hail,
                snow_fraction: 0.0,
                hail_fraction: 0.7,
                fog: 0.0,
                dust: 0.0,
                visibility_m: 700.0,
                wind_m_s: 18.0,
                wind_dir_deg: 250.0,
                gust_factor: 2.4,
                lightning_rate_per_min: 35.0,
                rainbow: 0.0,
                darkening: 0.72,
                ice_fraction: 0.45,
                profile: Deep,
            },
            Self::LightSnow => Target {
                temperature_offset_c: -20.0,
                humidity: 0.9,
                cloud_cover: 0.75,
                cloud_base_m: 700.0,
                cloud_top_m: 2_400.0,
                cloud_water: 0.7e-3,
                reff_m: 12.0e-6,
                precipitation_mm_h: 0.8,
                precipitation: P::Snow,
                snow_fraction: 1.0,
                hail_fraction: 0.0,
                fog: 0.05,
                dust: 0.0,
                visibility_m: 5_000.0,
                wind_m_s: 5.0,
                wind_dir_deg: 330.0,
                gust_factor: 1.2,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.2,
                ice_fraction: 0.65,
                profile: Snow,
            },
            Self::HeavySnow => Target {
                temperature_offset_c: -22.0,
                humidity: 0.95,
                cloud_cover: 0.95,
                cloud_base_m: 500.0,
                cloud_top_m: 3_500.0,
                cloud_water: 1.2e-3,
                reff_m: 16.0e-6,
                precipitation_mm_h: 5.0,
                precipitation: P::Snow,
                snow_fraction: 1.0,
                hail_fraction: 0.0,
                fog: 0.18,
                dust: 0.0,
                visibility_m: 1_800.0,
                wind_m_s: 9.0,
                wind_dir_deg: 320.0,
                gust_factor: 1.5,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.35,
                ice_fraction: 0.8,
                profile: Snow,
            },
            Self::Blizzard => Target {
                temperature_offset_c: -28.0,
                humidity: 0.93,
                cloud_cover: 1.0,
                cloud_base_m: 300.0,
                cloud_top_m: 3_000.0,
                cloud_water: 1.0e-3,
                reff_m: 18.0e-6,
                precipitation_mm_h: 7.0,
                precipitation: P::Snow,
                snow_fraction: 1.0,
                hail_fraction: 0.0,
                fog: 0.25,
                dust: 0.0,
                visibility_m: 300.0,
                wind_m_s: 22.0,
                wind_dir_deg: 310.0,
                gust_factor: 2.2,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.45,
                ice_fraction: 0.85,
                profile: Snow,
            },
            Self::Sleet => Target {
                temperature_offset_c: -16.0,
                humidity: 0.94,
                cloud_cover: 0.9,
                cloud_base_m: 600.0,
                cloud_top_m: 2_800.0,
                cloud_water: 1.1e-3,
                reff_m: 13.0e-6,
                precipitation_mm_h: 3.0,
                precipitation: P::Sleet,
                snow_fraction: 0.5,
                hail_fraction: 0.0,
                fog: 0.1,
                dust: 0.0,
                visibility_m: 3_000.0,
                wind_m_s: 7.0,
                wind_dir_deg: 330.0,
                gust_factor: 1.4,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.25,
                ice_fraction: 0.35,
                profile: Snow,
            },
            Self::FreezingRain => Target {
                temperature_offset_c: -18.0,
                humidity: 0.96,
                cloud_cover: 0.9,
                cloud_base_m: 400.0,
                cloud_top_m: 2_000.0,
                cloud_water: 1.0e-3,
                reff_m: 12.0e-6,
                precipitation_mm_h: 2.0,
                precipitation: P::FreezingRain,
                snow_fraction: 0.0,
                hail_fraction: 0.0,
                fog: 0.2,
                dust: 0.0,
                visibility_m: 2_500.0,
                wind_m_s: 5.0,
                wind_dir_deg: 340.0,
                gust_factor: 1.3,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.28,
                ice_fraction: 0.25,
                profile: Stratus,
            },
            Self::Gale => Target {
                temperature_offset_c: -1.0,
                humidity: 0.55,
                cloud_cover: 0.35,
                cloud_base_m: 1_400.0,
                cloud_top_m: 2_500.0,
                cloud_water: 0.4e-3,
                reff_m: 10.0e-6,
                precipitation_mm_h: 0.0,
                precipitation: P::None,
                snow_fraction: 0.0,
                hail_fraction: 0.0,
                fog: 0.0,
                dust: 0.05,
                visibility_m: 25_000.0,
                wind_m_s: 19.0,
                wind_dir_deg: 270.0,
                gust_factor: 2.0,
                lightning_rate_per_min: 0.0,
                rainbow: 0.0,
                darkening: 0.1,
                ice_fraction: 0.0,
                profile: Cumulus,
            },
        }
    }
}

/// Behaviour target for one weather state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    pub temperature_offset_c: f32,
    pub humidity: f32,
    pub cloud_cover: f32,
    pub cloud_base_m: f32,
    pub cloud_top_m: f32,
    pub cloud_water: f32,
    pub reff_m: f32,
    pub precipitation_mm_h: f32,
    pub precipitation: PrecipitationKind,
    pub snow_fraction: f32,
    pub hail_fraction: f32,
    pub fog: f32,
    pub dust: f32,
    pub visibility_m: f32,
    pub wind_m_s: f32,
    pub wind_dir_deg: f32,
    pub gust_factor: f32,
    pub lightning_rate_per_min: f32,
    pub rainbow: f32,
    pub darkening: f32,
    pub ice_fraction: f32,
    pub profile: CloudProfile,
}

impl Target {
    pub fn blended(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Self {
            temperature_offset_c: mix(self.temperature_offset_c, other.temperature_offset_c),
            humidity: mix(self.humidity, other.humidity),
            cloud_cover: mix(self.cloud_cover, other.cloud_cover),
            cloud_base_m: mix(self.cloud_base_m, other.cloud_base_m),
            cloud_top_m: mix(self.cloud_top_m, other.cloud_top_m),
            cloud_water: mix(self.cloud_water, other.cloud_water),
            reff_m: mix(self.reff_m, other.reff_m),
            precipitation_mm_h: mix(self.precipitation_mm_h, other.precipitation_mm_h),
            precipitation: if t < 0.5 {
                self.precipitation
            } else {
                other.precipitation
            },
            snow_fraction: mix(self.snow_fraction, other.snow_fraction),
            hail_fraction: mix(self.hail_fraction, other.hail_fraction),
            fog: mix(self.fog, other.fog),
            dust: mix(self.dust, other.dust),
            visibility_m: mix(self.visibility_m, other.visibility_m),
            wind_m_s: mix(self.wind_m_s, other.wind_m_s),
            wind_dir_deg: mix_angle(self.wind_dir_deg, other.wind_dir_deg, t),
            gust_factor: mix(self.gust_factor, other.gust_factor),
            lightning_rate_per_min: mix(self.lightning_rate_per_min, other.lightning_rate_per_min),
            rainbow: mix(self.rainbow, other.rainbow),
            darkening: mix(self.darkening, other.darkening),
            ice_fraction: mix(self.ice_fraction, other.ice_fraction),
            profile: if t < 0.5 { self.profile } else { other.profile },
        }
    }

    /// Enforce the few invariants that keep the catalogue self-consistent.
    pub fn sanitize(&mut self, temperature_c: f32, climate: Climate) {
        self.humidity = self.humidity.clamp(0.0, 1.0);
        self.cloud_cover = self.cloud_cover.clamp(0.0, 1.0);
        self.cloud_base_m = self.cloud_base_m.clamp(0.0, 15_000.0);
        self.cloud_top_m = self.cloud_top_m.clamp(self.cloud_base_m + 20.0, 18_000.0);
        self.cloud_water = self.cloud_water.clamp(0.0, 2.2e-4);
        self.precipitation_mm_h = self.precipitation_mm_h.clamp(0.0, 150.0);
        self.fog = self.fog.clamp(0.0, 1.0);
        self.dust = self.dust.clamp(0.0, 1.0);
        self.visibility_m = self.visibility_m.clamp(50.0, 80_000.0);
        self.wind_m_s = self.wind_m_s.clamp(0.0, 45.0);
        self.wind_dir_deg = self.wind_dir_deg.rem_euclid(360.0);
        self.gust_factor = self.gust_factor.clamp(1.0, 3.0);
        self.lightning_rate_per_min = self.lightning_rate_per_min.clamp(0.0, 60.0);
        self.rainbow = self.rainbow.clamp(0.0, 1.0);
        self.darkening = self.darkening.clamp(0.0, 1.0);
        self.ice_fraction = self.ice_fraction.clamp(0.0, 1.0);

        if self.cloud_cover < 0.02 {
            self.cloud_water = 0.0;
            self.precipitation_mm_h = 0.0;
            self.precipitation = PrecipitationKind::None;
            self.lightning_rate_per_min = 0.0;
        }
        let frozen = matches!(
            self.precipitation,
            PrecipitationKind::Snow | PrecipitationKind::Sleet
        );
        // Hail can fall through a warm surface layer, so it is allowed above
        // 4 C. Snow and sleet are not.
        if frozen && temperature_c > 4.0 {
            // A fixed state may ask for snow in summer; degrade rather than
            // render an impossible combination.
            self.precipitation = if temperature_c > 8.0 {
                PrecipitationKind::Rain
            } else {
                PrecipitationKind::Sleet
            };
            self.snow_fraction = if temperature_c > 8.0 { 0.0 } else { 0.5 };
            self.ice_fraction = self.ice_fraction.min(0.3);
        }
        if matches!(self.precipitation, PrecipitationKind::FreezingRain) && temperature_c > 1.0 {
            self.precipitation = PrecipitationKind::Rain;
        }
        if !climate.supports_snow() {
            self.snow_fraction = 0.0;
            if frozen || matches!(self.precipitation, PrecipitationKind::FreezingRain) {
                self.precipitation = PrecipitationKind::Rain;
                self.ice_fraction = self.ice_fraction.min(0.2);
            }
        }
        self.snow_fraction = self.snow_fraction.clamp(0.0, 1.0);
        self.hail_fraction = self.hail_fraction.clamp(0.0, 1.0);
    }
}

pub fn mix_angle(a: f32, b: f32, t: f32) -> f32 {
    let mut delta = (b - a).rem_euclid(360.0);
    if delta > 180.0 {
        delta -= 360.0;
    }
    (a + delta * t.clamp(0.0, 1.0)).rem_euclid(360.0)
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct WeatherKeyframe {
    pub time_s: f64,
    pub kind: WeatherKind,
    pub intensity: f32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum WeatherMode {
    #[default]
    Auto,
    Fixed(WeatherKind),
    Tour,
    Timeline(Vec<WeatherKeyframe>),
}

impl WeatherMode {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Fixed(_) => "Fixed",
            Self::Tour => "Tour",
            Self::Timeline(_) => "Timeline",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_kinds_have_finite_targets() {
        for kind in WeatherKind::ALL {
            let mut target = kind.target();
            target.sanitize(10.0, Climate::Temperate);
            assert!(target.cloud_cover.is_finite());
            assert!(target.precipitation_mm_h.is_finite());
            assert!(target.visibility_m >= 50.0);
            assert!(target.cloud_top_m >= target.cloud_base_m);
        }
    }

    #[test]
    fn cold_precip_is_degraded_in_warm_air() {
        let mut target = WeatherKind::LightSnow.target();
        target.sanitize(25.0, Climate::Temperate);
        assert!(matches!(
            target.precipitation,
            PrecipitationKind::Rain | PrecipitationKind::Sleet
        ));
        assert_eq!(target.snow_fraction, 0.0);
    }

    #[test]
    fn no_cloud_clears_precip() {
        let mut target = WeatherKind::Clear.target();
        target.cloud_cover = 0.0;
        target.sanitize(5.0, Climate::Temperate);
        assert_eq!(target.precipitation, PrecipitationKind::None);
    }

    #[test]
    fn angle_blend_takes_short_arc() {
        assert!((mix_angle(350.0, 10.0, 0.5) - 0.0).abs() < 0.01);
    }
}
