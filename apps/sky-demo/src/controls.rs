use crate::assets::RealtimeAsset;
use crate::color::DisplayTransform;
use crate::experiment::CompareMode;
use crate::view::ViewState;

pub const MIN_EXPOSURE_EV: f32 = -20.0;
pub const MAX_EXPOSURE_EV: f32 = 20.0;
pub const MIN_REINHARD_OVEREXPOSURE: f32 = 0.5;
pub const MAX_REINHARD_OVEREXPOSURE: f32 = 2.0;
pub const MIN_HDR_PAPER_WHITE_NITS: f32 = 80.0;
pub const MAX_HDR_PAPER_WHITE_NITS: f32 = 400.0;
pub const MIN_HDR_PEAK_NITS: f32 = 400.0;
pub const MAX_HDR_PEAK_NITS: f32 = 4_000.0;
pub const DEFAULT_REINHARD_OVEREXPOSURE: f32 = 1.1;
pub const DEFAULT_HDR_PAPER_WHITE_NITS: f32 = 203.0;
pub const DEFAULT_HDR_PEAK_NITS: f32 = 1_000.0;
pub const MIN_PLANET_RADIUS_KM: f32 = 1_000.0;
pub const MAX_PLANET_RADIUS_KM: f32 = 10_000.0;
pub const MIN_ATMOSPHERE_THICKNESS_KM: f32 = 1.0;
pub const MAX_ATMOSPHERE_THICKNESS_KM: f32 = 1_000.0;
pub const MIN_WEATHER_SPEED: f32 = 1.0;
pub const MAX_WEATHER_SPEED: f32 = 600.0;
pub const DEFAULT_WEATHER_SEED: u64 = 0xC10D_2026_1017;
/// Matches `sky_weather::Config::default()`; the workbench exposes it as a
/// behaviour-model control, not as an observational season.
pub const DEFAULT_WEATHER_SEASON: f32 = 0.55;
pub const DEFAULT_WEATHER_LATITUDE_DEG: f32 = 45.0;
pub const DEFAULT_WEATHER_DAY_OF_YEAR: f32 = 172.0;
pub const DEFAULT_WEATHER_START_HOUR_UTC: f32 = 9.0;

pub use sky_weather::{Climate, EffectsQuality, WeatherKind};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WeatherModeControl {
    #[default]
    Auto,
    Fixed,
    Tour,
}

impl WeatherModeControl {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Fixed, Self::Tour];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (evolving)",
            Self::Fixed => "Fixed weather",
            Self::Tour => "Catalogue tour",
        }
    }

    /// Chinese display label for the weather workbench UI.
    pub const fn label_zh(self) -> &'static str {
        match self {
            Self::Auto => "自动（演变）",
            Self::Fixed => "固定天气",
            Self::Tour => "目录轮巡",
        }
    }

    pub fn core_mode(self, kind: WeatherKind) -> sky_weather::WeatherMode {
        match self {
            Self::Auto => sky_weather::WeatherMode::Auto,
            Self::Fixed => sky_weather::WeatherMode::Fixed(kind),
            Self::Tour => sky_weather::WeatherMode::Tour,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudQuality {
    Balanced,
    High,
}

impl CloudQuality {
    pub const ALL: [Self; 2] = [Self::Balanced, Self::High];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Balanced => "Balanced (0.8x trace)",
            Self::High => "High (0.85x trace)",
        }
    }

    /// Chinese display label for the weather workbench UI.
    pub const fn label_zh(self) -> &'static str {
        match self {
            Self::Balanced => "平衡（0.8× 追踪）",
            Self::High => "高质量（0.85× 追踪）",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RealtimeControls {
    pub view: ViewState,
    pub compare_mode: CompareMode,
    pub sun_azimuth_deg: f32,
    pub sun_elevation_deg: f32,
    pub observer_altitude_km: f32,
    pub exposure_ev: f32,
    pub difference_scale: f32,
    pub tone_mapping_enabled: bool,
    pub hdr_enabled: bool,
    pub reinhard_overexposure: f32,
    pub hdr_paper_white_nits: f32,
    pub hdr_peak_nits: f32,
    pub planet_radius_km: f32,
    pub atmosphere_thickness_km: f32,
    pub aerosol_turbidity: f32,
    pub ground_albedo_spectral: [f32; 4],
    pub weather_enabled: bool,
    pub weather_mode: WeatherModeControl,
    pub weather_kind: WeatherKind,
    pub weather_climate: Climate,
    pub weather_intensity: f32,
    pub weather_effects: EffectsQuality,
    pub weather_speed: f32,
    pub weather_paused: bool,
    pub weather_seed: u64,
    /// 0 = winter, 0.5 = spring/autumn, 1 = summer (behaviour model input).
    pub weather_season: f32,
    pub weather_latitude_deg: f32,
    pub weather_day_of_year: f32,
    pub weather_start_hour_utc: f32,
    /// Scales the weather-driven overcast occlusion (0 = clear-sky background,
    /// 1 = automatic dimming from cloud coverage / liquid water path).
    pub cloud_occlusion: f32,
    pub auto_sun: bool,
    pub cloud_quality: CloudQuality,
}

impl RealtimeControls {
    pub fn from_asset(asset: &RealtimeAsset) -> Self {
        let manifest = asset.manifest();
        Self {
            view: ViewState::default(),
            compare_mode: CompareMode::default(),
            sun_azimuth_deg: manifest.sun_azimuth_deg,
            sun_elevation_deg: manifest.sun_elevation_deg,
            observer_altitude_km: manifest.observer_altitude_km,
            exposure_ev: 0.0,
            difference_scale: 4.0,
            tone_mapping_enabled: true,
            hdr_enabled: false,
            reinhard_overexposure: DEFAULT_REINHARD_OVEREXPOSURE,
            hdr_paper_white_nits: DEFAULT_HDR_PAPER_WHITE_NITS,
            hdr_peak_nits: DEFAULT_HDR_PEAK_NITS,
            planet_radius_km: 6360.0,
            atmosphere_thickness_km: 120.0,
            aerosol_turbidity: 1.0,
            ground_albedo_spectral: [0.18; 4],
            weather_enabled: true,
            weather_mode: WeatherModeControl::Auto,
            weather_kind: WeatherKind::PartlyCloudy,
            weather_climate: Climate::Temperate,
            weather_intensity: 1.0,
            weather_effects: EffectsQuality::High,
            weather_speed: 60.0,
            weather_paused: false,
            weather_seed: DEFAULT_WEATHER_SEED,
            weather_season: DEFAULT_WEATHER_SEASON,
            weather_latitude_deg: DEFAULT_WEATHER_LATITUDE_DEG,
            weather_day_of_year: DEFAULT_WEATHER_DAY_OF_YEAR,
            weather_start_hour_utc: DEFAULT_WEATHER_START_HOUR_UTC,
            cloud_occlusion: 1.0,
            auto_sun: true,
            cloud_quality: CloudQuality::Balanced,
        }
        .normalized()
    }

    pub fn normalized(mut self) -> Self {
        self.view.normalize();
        self.sun_azimuth_deg = self.sun_azimuth_deg.rem_euclid(360.0);
        self.sun_elevation_deg = self.sun_elevation_deg.clamp(-90.0, 90.0);
        self.exposure_ev = self.exposure_ev.clamp(MIN_EXPOSURE_EV, MAX_EXPOSURE_EV);
        self.difference_scale = self.difference_scale.clamp(0.25, 32.0);
        self.reinhard_overexposure = self
            .reinhard_overexposure
            .clamp(MIN_REINHARD_OVEREXPOSURE, MAX_REINHARD_OVEREXPOSURE);
        self.hdr_paper_white_nits = self
            .hdr_paper_white_nits
            .clamp(MIN_HDR_PAPER_WHITE_NITS, MAX_HDR_PAPER_WHITE_NITS);
        self.hdr_peak_nits = self
            .hdr_peak_nits
            .clamp(MIN_HDR_PEAK_NITS, MAX_HDR_PEAK_NITS)
            .max(self.hdr_paper_white_nits);
        self.planet_radius_km = self
            .planet_radius_km
            .clamp(MIN_PLANET_RADIUS_KM, MAX_PLANET_RADIUS_KM);
        self.atmosphere_thickness_km = self
            .atmosphere_thickness_km
            .clamp(MIN_ATMOSPHERE_THICKNESS_KM, MAX_ATMOSPHERE_THICKNESS_KM);
        self.observer_altitude_km = self
            .observer_altitude_km
            .clamp(0.0, self.maximum_observer_altitude_km());
        self.aerosol_turbidity = self.aerosol_turbidity.clamp(0.0, 10.0);
        for albedo in &mut self.ground_albedo_spectral {
            *albedo = albedo.clamp(0.0, 1.0);
        }
        self.weather_speed = self
            .weather_speed
            .clamp(MIN_WEATHER_SPEED, MAX_WEATHER_SPEED);
        self.weather_intensity = self.weather_intensity.clamp(0.25, 2.5);
        self.weather_season = self.weather_season.clamp(0.0, 1.0);
        self.weather_latitude_deg = self.weather_latitude_deg.clamp(-90.0, 90.0);
        self.weather_day_of_year = self.weather_day_of_year.clamp(1.0, 366.0);
        self.weather_start_hour_utc = self.weather_start_hour_utc.rem_euclid(24.0);
        self.cloud_occlusion = self.cloud_occlusion.clamp(0.0, 1.0);
        self
    }

    pub fn sync_asset_bound(&mut self, asset: &RealtimeAsset) {
        let manifest = asset.manifest();
        self.sun_azimuth_deg = manifest.sun_azimuth_deg.rem_euclid(360.0);
        self.sun_elevation_deg = manifest.sun_elevation_deg.clamp(-90.0, 90.0);
        self.observer_altitude_km = manifest
            .observer_altitude_km
            .clamp(0.0, self.maximum_observer_altitude_km());
    }

    pub fn reset_all(&mut self, asset: &RealtimeAsset) {
        *self = Self::from_asset(asset);
    }

    pub fn reset_view(&mut self) {
        self.view = ViewState::default();
    }

    pub fn reset_sun_observer(&mut self, asset: &RealtimeAsset) {
        self.sync_asset_bound(asset);
    }

    pub fn reset_display(&mut self) {
        self.compare_mode = CompareMode::default();
        self.exposure_ev = 0.0;
        self.difference_scale = 4.0;
        self.tone_mapping_enabled = true;
        self.hdr_enabled = false;
        self.reinhard_overexposure = DEFAULT_REINHARD_OVEREXPOSURE;
        self.hdr_paper_white_nits = DEFAULT_HDR_PAPER_WHITE_NITS;
        self.hdr_peak_nits = DEFAULT_HDR_PEAK_NITS;
    }

    /// Restore the behaviour-weather group only (used by the weather workbench
    /// "Reset Weather" action). View/display/sun inputs are left untouched.
    pub fn reset_weather(&mut self) {
        self.weather_enabled = true;
        self.weather_mode = WeatherModeControl::Auto;
        self.weather_kind = WeatherKind::PartlyCloudy;
        self.weather_climate = Climate::Temperate;
        self.weather_intensity = 1.0;
        self.weather_effects = EffectsQuality::High;
        self.weather_speed = 60.0;
        self.weather_paused = false;
        self.weather_seed = DEFAULT_WEATHER_SEED;
        self.weather_season = DEFAULT_WEATHER_SEASON;
        self.weather_latitude_deg = DEFAULT_WEATHER_LATITUDE_DEG;
        self.weather_day_of_year = DEFAULT_WEATHER_DAY_OF_YEAR;
        self.weather_start_hour_utc = DEFAULT_WEATHER_START_HOUR_UTC;
        self.cloud_occlusion = 1.0;
        self.auto_sun = true;
        self.cloud_quality = CloudQuality::Balanced;
    }

    pub fn maximum_observer_altitude_km(&self) -> f32 {
        100_000.0
    }

    pub fn exposure_multiplier(&self) -> f32 {
        DisplayTransform::default().exposure * self.exposure_ev.exp2()
    }

    pub fn hdr_paper_white_scale(&self) -> f32 {
        self.hdr_paper_white_nits / 80.0
    }

    pub fn hdr_peak_scale(&self) -> f32 {
        self.hdr_peak_nits / 80.0
    }
}

/// Chinese display labels for the workbench UI. The CLI keeps the documented
/// English names (`rain`, `heavy_snow`, ...); these helpers are display-only so
/// the solver crates stay language-neutral.
pub const fn weather_kind_label(kind: WeatherKind) -> &'static str {
    use WeatherKind as W;
    match kind {
        W::Clear => "晴空",
        W::Fair => "淡积云",
        W::PartlyCloudy => "多云",
        W::Overcast => "阴天",
        W::Fog => "雾",
        W::Haze => "霾",
        W::Dust => "沙尘",
        W::Drizzle => "毛毛雨",
        W::Rain => "雨",
        W::HeavyRain => "大雨",
        W::TropicalShower => "热带阵雨",
        W::Thunderstorm => "雷暴",
        W::Hailstorm => "冰雹",
        W::Supercell => "强风暴",
        W::LightSnow => "小雪",
        W::HeavySnow => "大雪",
        W::Blizzard => "暴风雪",
        W::Sleet => "雨夹雪",
        W::FreezingRain => "冻雨",
        W::Gale => "大风",
    }
}

pub const fn climate_label(climate: Climate) -> &'static str {
    use Climate as C;
    match climate {
        C::Tropical => "热带",
        C::Temperate => "温带",
        C::Continental => "大陆性",
        C::Arid => "干旱",
        C::Polar => "极地",
        C::Alpine => "高山",
    }
}

pub const fn effects_quality_label(quality: EffectsQuality) -> &'static str {
    match quality {
        EffectsQuality::High => "高",
        EffectsQuality::Low => "低",
    }
}

pub const fn precipitation_kind_label(kind: sky_weather::PrecipitationKind) -> &'static str {
    use sky_weather::PrecipitationKind as P;
    match kind {
        P::None => "无降水",
        P::Rain => "雨",
        P::Snow => "雪",
        P::Sleet => "雨夹雪",
        P::Hail => "冰雹",
        P::FreezingRain => "冻雨",
    }
}

/// Parse a documented weather-kind CLI name (for example `rain` or
/// `heavy_snow`). Kept next to the control enum so snapshot and workbench
/// entry points share one spelling.
pub fn weather_kind_from_name(name: &str) -> Option<WeatherKind> {
    use WeatherKind as W;
    Some(match name {
        "clear" => W::Clear,
        "fair" => W::Fair,
        "partly_cloudy" => W::PartlyCloudy,
        "overcast" => W::Overcast,
        "fog" => W::Fog,
        "haze" => W::Haze,
        "dust" => W::Dust,
        "drizzle" => W::Drizzle,
        "rain" => W::Rain,
        "heavy_rain" => W::HeavyRain,
        "tropical_shower" => W::TropicalShower,
        "thunderstorm" => W::Thunderstorm,
        "hailstorm" => W::Hailstorm,
        "supercell" => W::Supercell,
        "light_snow" => W::LightSnow,
        "heavy_snow" => W::HeavySnow,
        "blizzard" => W::Blizzard,
        "sleet" => W::Sleet,
        "freezing_rain" => W::FreezingRain,
        "gale" => W::Gale,
        _ => return None,
    })
}

/// Parse a documented climate CLI name.
pub fn climate_from_name(name: &str) -> Option<Climate> {
    use Climate as C;
    Some(match name {
        "tropical" => C::Tropical,
        "temperate" => C::Temperate,
        "continental" => C::Continental,
        "arid" => C::Arid,
        "polar" => C::Polar,
        "alpine" => C::Alpine,
        _ => return None,
    })
}

/// Parse a weather-mode CLI name (`auto`, `fixed`, `tour`).
pub fn weather_mode_from_name(name: &str) -> Option<WeatherModeControl> {
    Some(match name {
        "auto" => WeatherModeControl::Auto,
        "fixed" => WeatherModeControl::Fixed,
        "tour" => WeatherModeControl::Tour,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use sky_assets::asset::{SpectralAssetFiles, SpectralAssetManifest};

    use super::{MAX_EXPOSURE_EV, RealtimeControls};
    use crate::assets::RealtimeAsset;

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

    fn test_asset() -> RealtimeAsset {
        let root = std::env::temp_dir().join(format!(
            "sky-realtime-controls-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create test root");
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
        let path = root.join("asset.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&manifest).expect("serialize manifest"),
        )
        .expect("write manifest");
        RealtimeAsset::load(path).expect("load test asset")
    }

    #[test]
    fn exposure_ev_is_relative_to_the_current_display_baseline() {
        let asset = test_asset();
        let mut controls = RealtimeControls::from_asset(&asset);
        let baseline = controls.exposure_multiplier();
        controls.exposure_ev = 1.0;
        assert!((controls.exposure_multiplier() - baseline * 2.0).abs() < 1.0e-6);
    }

    #[test]
    fn normalization_clamps_unsafe_values() {
        let asset = test_asset();
        let mut controls = RealtimeControls::from_asset(&asset);
        controls.exposure_ev = 100.0;
        controls.sun_elevation_deg = -120.0;
        controls.atmosphere_thickness_km = 5.0;
        controls.observer_altitude_km = 20.0;
        controls.reinhard_overexposure = 10.0;
        controls.hdr_paper_white_nits = 1_000.0;
        controls.hdr_peak_nits = 100.0;
        controls = controls.normalized();
        assert_eq!(controls.exposure_ev, MAX_EXPOSURE_EV);
        assert_eq!(controls.sun_elevation_deg, -90.0);
        assert_eq!(controls.observer_altitude_km, 20.0);
        assert_eq!(controls.reinhard_overexposure, 2.0);
        assert_eq!(controls.hdr_paper_white_nits, 400.0);
        assert_eq!(controls.hdr_peak_nits, 400.0);
    }

    #[test]
    fn asset_switch_only_updates_asset_bound_controls() {
        let asset = test_asset();
        let mut controls = RealtimeControls::from_asset(&asset);
        controls.view.yaw_deg = 90.0;
        controls.exposure_ev = 2.0;
        controls.aerosol_turbidity = 4.0;
        controls.sun_elevation_deg = -10.0;
        controls.sync_asset_bound(&asset);
        assert_eq!(controls.view.yaw_deg, 90.0);
        assert_eq!(controls.exposure_ev, 2.0);
        assert_eq!(controls.aerosol_turbidity, 4.0);
        assert_eq!(controls.sun_elevation_deg, 12.0);
    }

    #[test]
    fn group_and_global_resets_restore_their_expected_scope() {
        let asset = test_asset();
        let defaults = RealtimeControls::from_asset(&asset);
        let mut controls = defaults;
        controls.view.yaw_deg = 90.0;
        controls.exposure_ev = 3.0;
        controls.difference_scale = 16.0;
        controls.tone_mapping_enabled = false;
        controls.hdr_enabled = true;
        controls.reinhard_overexposure = 1.8;
        controls.hdr_paper_white_nits = 300.0;
        controls.aerosol_turbidity = 8.0;

        controls.reset_display();
        assert_eq!(controls.view.yaw_deg, 90.0);
        assert_eq!(controls.exposure_ev, defaults.exposure_ev);
        assert_eq!(controls.difference_scale, defaults.difference_scale);
        assert!(controls.tone_mapping_enabled);
        assert!(!controls.hdr_enabled);
        assert_eq!(
            controls.reinhard_overexposure,
            defaults.reinhard_overexposure
        );
        assert_eq!(controls.hdr_paper_white_nits, defaults.hdr_paper_white_nits);
        assert_eq!(controls.aerosol_turbidity, 8.0);

        controls.reset_all(&asset);
        assert_eq!(controls, defaults);
    }

    #[test]
    fn hdr_scales_use_the_scrgb_eighty_nit_reference() {
        let asset = test_asset();
        let controls = RealtimeControls::from_asset(&asset);
        assert_eq!(controls.hdr_paper_white_scale(), 203.0 / 80.0);
        assert_eq!(controls.hdr_peak_scale(), 1000.0 / 80.0);
    }

    #[test]
    fn weather_group_reset_leaves_view_and_display_alone() {
        let asset = test_asset();
        let mut controls = RealtimeControls::from_asset(&asset);
        controls.view.yaw_deg = 90.0;
        controls.exposure_ev = 2.0;
        controls.weather_mode = super::WeatherModeControl::Fixed;
        controls.weather_kind = super::WeatherKind::Thunderstorm;
        controls.weather_season = 0.1;
        controls.weather_latitude_deg = -10.0;
        controls.weather_day_of_year = 10.0;
        controls.weather_start_hour_utc = 3.0;
        controls.reset_weather();
        assert_eq!(controls.view.yaw_deg, 90.0);
        assert_eq!(controls.exposure_ev, 2.0);
        assert_eq!(controls.weather_mode, super::WeatherModeControl::Auto);
        assert_eq!(controls.weather_kind, super::WeatherKind::PartlyCloudy);
        assert_eq!(controls.weather_season, super::DEFAULT_WEATHER_SEASON);
        assert_eq!(
            controls.weather_latitude_deg,
            super::DEFAULT_WEATHER_LATITUDE_DEG
        );
        assert_eq!(
            controls.weather_day_of_year,
            super::DEFAULT_WEATHER_DAY_OF_YEAR
        );
        assert_eq!(
            controls.weather_start_hour_utc,
            super::DEFAULT_WEATHER_START_HOUR_UTC
        );
    }

    #[test]
    fn weather_site_and_season_inputs_are_normalized() {
        let asset = test_asset();
        let mut controls = RealtimeControls::from_asset(&asset);
        controls.weather_season = -2.0;
        controls.weather_latitude_deg = 200.0;
        controls.weather_day_of_year = 500.0;
        controls.weather_start_hour_utc = 27.5;
        controls.cloud_occlusion = 5.0;
        controls = controls.normalized();
        assert_eq!(controls.weather_season, 0.0);
        assert_eq!(controls.weather_latitude_deg, 90.0);
        assert_eq!(controls.weather_day_of_year, 366.0);
        assert_eq!(controls.weather_start_hour_utc, 3.5);
        assert_eq!(controls.cloud_occlusion, 1.0);
    }

    #[test]
    fn documented_weather_and_climate_names_parse() {
        for name in [
            "clear",
            "fair",
            "partly_cloudy",
            "overcast",
            "fog",
            "haze",
            "dust",
            "drizzle",
            "rain",
            "heavy_rain",
            "tropical_shower",
            "thunderstorm",
            "hailstorm",
            "supercell",
            "light_snow",
            "heavy_snow",
            "blizzard",
            "sleet",
            "freezing_rain",
            "gale",
        ] {
            assert!(super::weather_kind_from_name(name).is_some(), "{name}");
        }
        assert!(super::weather_kind_from_name("tornado").is_none());
        for name in [
            "tropical",
            "temperate",
            "continental",
            "arid",
            "polar",
            "alpine",
        ] {
            assert!(super::climate_from_name(name).is_some(), "{name}");
        }
        assert!(super::climate_from_name("mars").is_none());
        assert_eq!(
            super::weather_mode_from_name("fixed"),
            Some(super::WeatherModeControl::Fixed)
        );
    }

    #[test]
    fn chinese_display_labels_cover_the_weather_catalogue() {
        let mut seen = std::collections::HashSet::new();
        for kind in super::WeatherKind::ALL {
            let label = super::weather_kind_label(kind);
            assert!(!label.is_empty());
            assert!(seen.insert(label), "duplicate label {label}");
        }
        let mut seen = std::collections::HashSet::new();
        for climate in super::Climate::ALL {
            assert!(!super::climate_label(climate).is_empty());
            assert!(seen.insert(super::climate_label(climate)));
        }
        assert_eq!(
            super::effects_quality_label(super::EffectsQuality::High),
            "高"
        );
        assert_eq!(
            super::precipitation_kind_label(sky_weather::PrecipitationKind::Snow),
            "雪"
        );
        assert_eq!(super::WeatherModeControl::Fixed.label_zh(), "固定天气");
        assert_eq!(super::CloudQuality::High.label_zh(), "高质量（0.85× 追踪）");
        // The reference/PT workbench keeps its English labels.
        assert_eq!(super::WeatherModeControl::Fixed.label(), "Fixed weather");
    }
}
