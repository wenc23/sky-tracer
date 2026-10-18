//! Fast 2D behaviour field and director orchestration.
//!
//! One column per grid cell. The field is advected by a local wind and relaxed
//! toward weather-state targets. This is deliberately not a primitive-equation
//! model; it is the smallest core that can express the full behaviour catalogue
//! smoothly and at interactive cost.

use serde::{Deserialize, Serialize};

use crate::{
    CloudVolume, Config, EffectsState, Result, Snapshot, SunPosition, WeatherDirector,
    WeatherFrame, WeatherKind, WeatherMode,
    effects::Precipitation,
    kind::{CloudProfile, PrecipitationKind, Target},
    solar, thermo,
};

const P_SURFACE_PA: f64 = 101_325.0;
const RHO_AIR: f64 = 1.12;
const T0_K: f64 = 273.15;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Column {
    pub temperature_c: f32,
    pub humidity: f32,
    pub cloud_water: f32,
    pub cloud_fraction: f32,
    pub cloud_base_m: f32,
    pub cloud_top_m: f32,
    pub reff_m: f32,
    pub ice_fraction: f32,
    pub precip_mm_h: f32,
    pub precip_kind: PrecipitationKind,
    pub snow_fraction: f32,
    pub hail_fraction: f32,
    pub fog_density: f32,
    pub dust_density: f32,
    pub visibility_m: f32,
    pub wind_u_m_s: f32,
    pub wind_v_m_s: f32,
    pub wind_speed_m_s: f32,
    pub wind_dir_deg: f32,
    pub gust_m_s: f32,
    pub lightning_rate_per_min: f32,
    pub rainbow: f32,
    pub cloud_darkening: f32,
    pub profile: CloudProfile,
}

impl Default for Column {
    fn default() -> Self {
        Self {
            temperature_c: 15.0,
            humidity: 0.5,
            cloud_water: 0.0,
            cloud_fraction: 0.0,
            cloud_base_m: 1_000.0,
            cloud_top_m: 1_020.0,
            reff_m: 10.0e-6,
            ice_fraction: 0.0,
            precip_mm_h: 0.0,
            precip_kind: PrecipitationKind::None,
            snow_fraction: 0.0,
            hail_fraction: 0.0,
            fog_density: 0.0,
            dust_density: 0.0,
            visibility_m: 50_000.0,
            wind_u_m_s: 0.0,
            wind_v_m_s: 0.0,
            wind_speed_m_s: 0.0,
            wind_dir_deg: 180.0,
            gust_m_s: 0.0,
            lightning_rate_per_min: 0.0,
            rainbow: 0.0,
            cloud_darkening: 0.0,
            profile: CloudProfile::None,
        }
    }
}

/// Weather model state. All arrays are row-major with x fastest.
///
/// `Clone` produces a full, independent snapshot of the behaviour state. The
/// real-time workbench relies on this to hand the `sample_volume` reconstruction
/// to a worker thread while the render thread keeps advancing the live model.
#[derive(Clone)]
pub struct Model {
    pub(crate) config: Config,
    pub(crate) time_s: f64,
    pub(crate) sun_override: Option<[f64; 2]>,
    pub(crate) director: WeatherDirector,
    pub(crate) columns: Vec<Column>,
    pub(crate) wind_u: Vec<f32>,
    pub(crate) wind_v: Vec<f32>,
    pub(crate) accumulator_s: f64,
}

impl Model {
    pub fn new(config: Config) -> Result<Self> {
        config.validate()?;
        let kind = match &config.mode {
            WeatherMode::Fixed(kind) => *kind,
            WeatherMode::Timeline(frames) => frames.first().map_or(WeatherKind::Clear, |f| f.kind),
            WeatherMode::Tour => WeatherKind::Clear,
            WeatherMode::Auto => WeatherKind::PartlyCloudy,
        };
        let director =
            WeatherDirector::new(config.mode.clone(), kind, config.intensity, config.seed);
        let mut target = director.target();
        let sun = SunPosition {
            elevation_deg: config.sun_elevation_deg,
            azimuth_deg: config.sun_azimuth_deg,
        };
        let initial_base = config.climate.base_temperature_c()
            + (config.season - 0.5) * 14.0
            + (sun.elevation_deg as f32 / 90.0).max(-0.2) * 3.0;
        target.sanitize(initial_base + target.temperature_offset_c, config.climate);
        let n = config.grid[0] as usize * config.grid[1] as usize;
        let mut columns = Vec::with_capacity(n);
        let mut wind_u = Vec::with_capacity(n);
        let mut wind_v = Vec::with_capacity(n);
        for y in 0..config.grid[1] as usize {
            for x in 0..config.grid[0] as usize {
                let n1 = hash01(config.seed, x as u32, y as u32);
                let n2 = hash01(config.seed ^ 0x5EED, x as u32, y as u32);
                let wx = x as f32 * config.dx_m as f32 - config.domain_m() * 0.5;
                let wz = y as f32 * config.dx_m as f32 - config.domain_m() * 0.5;
                let mut column = column_from_target(
                    &config,
                    target,
                    sun,
                    n1,
                    n2,
                    director.system_influence(wx, wz),
                );
                let (u, v) = wind_components(column.wind_dir_deg, column.wind_speed_m_s);
                column.wind_u_m_s = u;
                column.wind_v_m_s = v;
                wind_u.push(u);
                wind_v.push(v);
                columns.push(column);
            }
        }
        Ok(Self {
            config,
            time_s: 0.0,
            sun_override: None,
            director,
            columns,
            wind_u,
            wind_v,
            accumulator_s: 0.0,
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn time_s(&self) -> f64 {
        self.time_s
    }

    /// Advance exactly one fixed model step (`config.dt_s`).
    ///
    /// Applications should normally call [`Self::advance`] with a
    /// wall-clock-derived number of simulated seconds instead. Calling `step`
    /// once per presented frame makes the simulated rate depend on the frame
    /// rate: 60 step/s is 60 simulated seconds per wall second, 144 step/s is
    /// twice that.
    pub fn step(&mut self) -> Result<()> {
        self.advance(self.config.dt_s)
    }

    /// Advance the model by `seconds` of simulated time, using a fixed-step
    /// accumulator.
    ///
    /// Time is accumulated and consumed in whole `config.dt_s` steps; an
    /// incomplete remainder is carried into the next call. The model therefore
    /// only ever sees steps of the same fixed size, whatever chunk sizes the
    /// caller passes: `advance(10.0)` and ten `advance(1.0)` calls (or a run of
    /// jittery frame-sized chunks) produce the same sequence of model steps.
    /// Frame rate may select *which* chunk boundaries the steps happen to land
    /// on, but not the long-run simulated rate; the only effect is a sub-step
    /// phase lag of less than `dt_s` of simulated time.
    ///
    /// `seconds` must be finite and within `0..=600`, and a single call must
    /// stay within an internal 32_000-step budget (600 s with the default
    /// `dt_s = 1.0` is 600 steps). Callers that need to catch up more simulated
    /// time than that must split it into multiple calls.
    pub fn advance(&mut self, seconds: f64) -> Result<()> {
        if !seconds.is_finite() || !(0.0..=600.0).contains(&seconds) {
            return Err("advance chunk must be finite and within 0..=600 s".into());
        }
        self.accumulator_s += seconds;
        let dt = self.config.dt_s;
        let mut steps = 0u32;
        while self.accumulator_s + 1.0e-9 >= dt {
            self.step_internal(dt as f32)?;
            self.accumulator_s -= dt;
            steps += 1;
            if steps > 32_000 {
                return Err("weather advance exceeded substep budget".into());
            }
        }
        Ok(())
    }

    fn step_internal(&mut self, dt_s: f32) -> Result<()> {
        let nx = self.config.grid[0] as usize;
        let ny = self.config.grid[1] as usize;
        let dx = self.config.dx_m as f32;
        let sun = self.sun();
        self.director.step(
            dt_s,
            self.time_s,
            self.config.climate,
            self.config.season,
            self.config.domain_m(),
        );
        let mut target = self.director.target();
        let base_temperature = self.config.climate.base_temperature_c()
            + (self.config.season - 0.5) * 14.0
            + (sun.elevation_deg as f32 / 90.0).max(-0.2) * 3.0;
        target.sanitize(
            base_temperature + target.temperature_offset_c,
            self.config.climate,
        );

        let temp = advect_scalar(
            &self.columns,
            nx,
            ny,
            dx,
            dt_s,
            &self.wind_u,
            &self.wind_v,
            |c| c.temperature_c,
        );
        let humidity = advect_scalar(
            &self.columns,
            nx,
            ny,
            dx,
            dt_s,
            &self.wind_u,
            &self.wind_v,
            |c| c.humidity,
        );
        let cloud = advect_scalar(
            &self.columns,
            nx,
            ny,
            dx,
            dt_s,
            &self.wind_u,
            &self.wind_v,
            |c| c.cloud_water,
        );
        let precip = advect_scalar(
            &self.columns,
            nx,
            ny,
            dx,
            dt_s,
            &self.wind_u,
            &self.wind_v,
            |c| c.precip_mm_h,
        );
        let fog = advect_scalar(
            &self.columns,
            nx,
            ny,
            dx,
            dt_s,
            &self.wind_u,
            &self.wind_v,
            |c| c.fog_density,
        );
        let dust = advect_scalar(
            &self.columns,
            nx,
            ny,
            dx,
            dt_s,
            &self.wind_u,
            &self.wind_v,
            |c| c.dust_density,
        );

        for y in 0..ny {
            for x in 0..nx {
                let index = y * nx + x;
                let wx = x as f32 * dx - self.config.domain_m() * 0.5;
                let wz = y as f32 * dx - self.config.domain_m() * 0.5;
                let system = self.director.system_influence(wx, wz);
                let n1 = hash01(self.config.seed, x as u32, y as u32);
                let n2 = hash01(
                    self.config.seed ^ 0x9E37,
                    (x as u32).wrapping_add((self.time_s / 17.0) as u32),
                    y as u32,
                );
                let c = &mut self.columns[index];
                let target_cloud = (target.cloud_cover * (0.72 + 0.55 * n1)
                    + system * (1.0 - target.cloud_cover) * 0.35)
                    .clamp(0.0, 1.0);
                let target_precip =
                    target.precipitation_mm_h * (0.55 + 0.9 * n1) * (0.45 + 1.75 * system.max(0.0));
                let target_fog = target.fog * (0.6 + 0.8 * n2);
                let target_dust = target.dust * (0.6 + 0.8 * n1);

                c.temperature_c = relax(
                    temp[index],
                    target.temperature_c(base_temperature) + (n1 - 0.5) * 1.2,
                    4.0,
                    dt_s,
                )
                .clamp(-55.0, 55.0);
                c.humidity = relax(
                    humidity[index],
                    (target.humidity + (n2 - 0.5) * 0.08).clamp(0.02, 1.0),
                    4.0,
                    dt_s,
                )
                .clamp(0.01, 1.0);
                c.cloud_fraction = relax(c.cloud_fraction, target_cloud, 5.0, dt_s);
                c.cloud_water = relax(
                    cloud[index],
                    target.cloud_water * (0.7 + 0.6 * n1),
                    3.0,
                    dt_s,
                )
                .clamp(0.0, 8.0e-3);
                c.cloud_base_m = relax(
                    c.cloud_base_m,
                    target.cloud_base_m + (n1 - 0.5) * 120.0,
                    5.0,
                    dt_s,
                )
                .clamp(30.0, 15_000.0);
                c.cloud_top_m = relax(
                    c.cloud_top_m,
                    (target.cloud_top_m + (n2 - 0.5) * 300.0).max(c.cloud_base_m + 30.0),
                    5.0,
                    dt_s,
                )
                .clamp(c.cloud_base_m + 30.0, 18_000.0);
                c.reff_m = relax(c.reff_m, target.reff_m, 5.0, dt_s).clamp(3.0e-6, 80.0e-6);
                c.ice_fraction = relax(c.ice_fraction, target.ice_fraction, 4.0, dt_s);
                c.precip_mm_h = relax(precip[index], target_precip, 6.0, dt_s);
                c.precip_kind = if c.precip_mm_h > 0.02 && c.cloud_fraction > 0.03 {
                    target.precipitation
                } else {
                    PrecipitationKind::None
                };
                if matches!(
                    c.precip_kind,
                    PrecipitationKind::Snow | PrecipitationKind::Sleet
                ) && c.temperature_c > 4.0
                {
                    c.precip_kind = if c.temperature_c > 8.0 {
                        PrecipitationKind::Rain
                    } else {
                        PrecipitationKind::Sleet
                    };
                }
                if matches!(c.precip_kind, PrecipitationKind::FreezingRain) && c.temperature_c > 1.0
                {
                    c.precip_kind = PrecipitationKind::Rain;
                }
                c.snow_fraction = if matches!(
                    c.precip_kind,
                    PrecipitationKind::Snow | PrecipitationKind::Sleet
                ) {
                    target.snow_fraction
                } else {
                    0.0
                };
                c.hail_fraction = target.hail_fraction;
                c.fog_density = relax(fog[index], target_fog, 5.0, dt_s).clamp(0.0, 1.0);
                c.dust_density = relax(dust[index], target_dust, 5.0, dt_s).clamp(0.0, 1.0);
                c.visibility_m = (target.visibility_m
                    * (1.0 - 0.6 * c.fog_density)
                    * (1.0 - 0.5 * c.dust_density))
                    .clamp(40.0, 80_000.0);
                c.wind_dir_deg = {
                    let current = c.wind_dir_deg;
                    crate::kind::mix_angle(current, target.wind_dir_deg, (dt_s / 25.0).min(1.0))
                };
                c.wind_speed_m_s = relax(
                    c.wind_speed_m_s,
                    target.wind_m_s * (0.75 + 0.35 * n1),
                    3.0,
                    dt_s,
                )
                .clamp(0.0, 45.0);
                let (u, v) = wind_components(c.wind_dir_deg, c.wind_speed_m_s);
                c.wind_u_m_s = u;
                c.wind_v_m_s = v;
                self.wind_u[index] = u;
                self.wind_v[index] = v;
                c.gust_m_s = (c.wind_speed_m_s * target.gust_factor).clamp(0.0, 55.0);
                c.lightning_rate_per_min = relax(
                    c.lightning_rate_per_min,
                    target.lightning_rate_per_min * (0.5 + 1.5 * system),
                    4.0,
                    dt_s,
                )
                .clamp(0.0, 60.0);
                c.rainbow = relax(c.rainbow, target.rainbow, 6.0, dt_s).clamp(0.0, 1.0);
                c.cloud_darkening = relax(c.cloud_darkening, target.darkening, 4.0, dt_s);
                c.profile = target.profile;
                if c.cloud_fraction < 0.02 {
                    c.cloud_water = 0.0;
                    c.precip_mm_h = 0.0;
                    c.precip_kind = PrecipitationKind::None;
                    c.lightning_rate_per_min = 0.0;
                }
                if !c.temperature_c.is_finite()
                    || !c.humidity.is_finite()
                    || !c.cloud_water.is_finite()
                    || !c.precip_mm_h.is_finite()
                    || !c.visibility_m.is_finite()
                {
                    return Err("weather field became non-finite".into());
                }
            }
        }
        self.time_s += f64::from(dt_s);
        Ok(())
    }

    pub fn set_sun(&mut self, elevation_deg: f64, azimuth_deg: f64) -> Result<()> {
        if !elevation_deg.is_finite()
            || !(-90.0..=90.0).contains(&elevation_deg)
            || !azimuth_deg.is_finite()
            || !(0.0..=360.0).contains(&azimuth_deg)
        {
            return Err("sun override must be finite and in range".into());
        }
        self.sun_override = Some([elevation_deg, azimuth_deg.rem_euclid(360.0)]);
        Ok(())
    }

    pub fn sun(&self) -> SunPosition {
        if self.config.auto_sun {
            let hours = self.config.start_hour_utc + self.time_s / 3600.0;
            solar::position(
                self.config.latitude_deg,
                self.config.longitude_deg,
                self.config.day_of_year,
                hours,
            )
        } else {
            let [elevation, azimuth] = self
                .sun_override
                .unwrap_or([self.config.sun_elevation_deg, self.config.sun_azimuth_deg]);
            SunPosition {
                elevation_deg: elevation,
                azimuth_deg: azimuth,
            }
        }
    }

    pub fn set_mode(&mut self, mode: WeatherMode) -> Result<()> {
        self.director.set_mode(mode).map_err(|error| error.into())
    }

    pub fn set_weather(&mut self, kind: WeatherKind) -> Result<()> {
        self.set_mode(WeatherMode::Fixed(kind))
    }

    pub fn set_intensity(&mut self, intensity: f32) -> Result<()> {
        if !intensity.is_finite() || !(0.25..=2.5).contains(&intensity) {
            return Err("weather intensity must be finite and in 0.25..=2.5".into());
        }
        self.config.intensity = intensity;
        self.director.set_intensity(intensity);
        Ok(())
    }

    pub fn frame(&self) -> WeatherFrame {
        let sun = self.sun();
        let count = self.columns.len().max(1) as f64;
        let mut temperature = 0.0;
        let mut humidity = 0.0;
        let mut cloud_cover = 0.0;
        let mut lwp = 0.0;
        let mut base = 0.0;
        let mut top = 0.0;
        let mut cloudy = 0.0;
        let mut precip = 0.0;
        let mut fog = 0.0;
        let mut dust = 0.0;
        let mut visibility = 0.0;
        let mut wind = 0.0;
        let mut gust = 0.0;
        let mut wind_u = 0.0;
        let mut wind_v = 0.0;
        let mut lightning = 0.0;
        let mut rainbow = 0.0;
        let mut darkening = 0.0;
        let mut kind = PrecipitationKind::None;
        let mut dominant_rate = 0.0f32;
        let mut snow_fraction = 0.0;
        let mut hail_fraction = 0.0;
        for column in &self.columns {
            temperature += f64::from(column.temperature_c);
            humidity += f64::from(column.humidity);
            cloud_cover += f64::from(column.cloud_fraction);
            let depth = f64::from((column.cloud_top_m - column.cloud_base_m).max(0.0));
            lwp += RHO_AIR
                * f64::from(column.cloud_water)
                * depth
                * f64::from(column.cloud_fraction)
                * 1_000.0;
            if column.cloud_fraction > 0.03 {
                base += f64::from(column.cloud_base_m);
                top += f64::from(column.cloud_top_m);
                cloudy += 1.0;
            }
            precip += f64::from(column.precip_mm_h);
            if column.precip_mm_h > dominant_rate && column.precip_kind != PrecipitationKind::None {
                dominant_rate = column.precip_mm_h;
                kind = column.precip_kind;
            }
            snow_fraction += f64::from(column.snow_fraction);
            hail_fraction += f64::from(column.hail_fraction);
            fog += f64::from(column.fog_density);
            dust += f64::from(column.dust_density);
            visibility += f64::from(column.visibility_m);
            wind += f64::from(column.wind_speed_m_s);
            gust += f64::from(column.gust_m_s);
            wind_u += f64::from(column.wind_u_m_s);
            wind_v += f64::from(column.wind_v_m_s);
            lightning += f64::from(column.lightning_rate_per_min);
            rainbow += f64::from(column.rainbow);
            darkening += f64::from(column.cloud_darkening);
        }
        let temp_c = temperature / count;
        let rh = (humidity / count).clamp(0.01, 1.0);
        let q = thermo::saturation_specific_humidity(temp_c + T0_K, P_SURFACE_PA) * rh;
        let dew_point = thermo::dew_point(q, P_SURFACE_PA) - T0_K;
        let mean_wind = wind / count;
        let mean_dir =
            ((wind_v / count).atan2(wind_u / count).to_degrees() + 90.0).rem_euclid(360.0);
        let mean_precip = precip / count;
        let intensity = (mean_precip / 25.0).clamp(0.0, 1.0) as f32;
        WeatherFrame {
            time_s: self.time_s,
            kind: self.director.kind,
            intensity: self.director.intensity,
            temperature_c: temp_c,
            dew_point_c: dew_point,
            humidity: rh,
            pressure_hpa: 1013.25,
            wind_speed_m_s: mean_wind,
            wind_dir_deg: mean_dir,
            gust_m_s: gust / count,
            cloud_cover: cloud_cover / count,
            lwp_g_m2: lwp / count,
            cloud_base_m: if cloudy > 0.0 { base / cloudy } else { 0.0 },
            cloud_top_m: if cloudy > 0.0 { top / cloudy } else { 0.0 },
            precipitation: Precipitation {
                kind: if mean_precip > 0.02 {
                    kind
                } else {
                    PrecipitationKind::None
                },
                rate_mm_h: mean_precip,
                intensity,
                snow_fraction: (snow_fraction / count) as f32,
                hail_fraction: (hail_fraction / count) as f32,
            },
            fog_density: fog / count,
            dust_density: dust / count,
            visibility_m: visibility / count,
            lightning_rate_per_min: lightning / count,
            rainbow_strength: rainbow / count,
            cloud_darkening: darkening / count,
            sun,
        }
    }

    pub fn diagnostics(&self) -> WeatherFrame {
        self.frame()
    }

    pub fn effects(&self) -> EffectsState {
        let frame = self.frame();
        let kind = frame.precipitation.kind;
        let rate = frame.precipitation.rate_mm_h as f32;
        let normalization = 8.0;
        let rain = if matches!(
            kind,
            PrecipitationKind::Rain | PrecipitationKind::FreezingRain
        ) {
            (rate / normalization).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let snow = if matches!(kind, PrecipitationKind::Snow) {
            (rate / (normalization * 0.5)).clamp(0.0, 1.0)
        } else if matches!(kind, PrecipitationKind::Sleet) {
            (rate / normalization).clamp(0.0, 0.7)
        } else {
            0.0
        };
        let hail = if matches!(kind, PrecipitationKind::Hail) {
            (rate / normalization).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let sleet = if matches!(kind, PrecipitationKind::Sleet) {
            (rate / normalization).clamp(0.0, 0.8)
        } else {
            0.0
        };
        let angle = (frame.wind_dir_deg as f32).to_radians();
        let wind_scale = (frame.wind_speed_m_s as f32 / 25.0).clamp(0.0, 1.0);
        let (flash, seed) =
            lightning_pulse(frame.time_s, frame.lightning_rate_per_min, self.config.seed);
        EffectsState {
            time_s: frame.time_s as f32,
            rain,
            snow,
            hail,
            sleet,
            fog: frame.fog_density as f32,
            dust: frame.dust_density as f32,
            wind: [angle.sin() * wind_scale, angle.cos() * wind_scale],
            cloud_darkening: frame.cloud_darkening as f32,
            rainbow: frame.rainbow_strength as f32,
            lightning_flash: flash,
            lightning_seed: seed,
        }
        .sanitize()
    }

    pub fn sample_volume(&self) -> Result<CloudVolume> {
        crate::volume::sample(self)
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            config: self.config.clone(),
            time_s: self.time_s,
            sun_override: self.sun_override,
            director: self.director.clone(),
            columns: self.columns.clone(),
            wind_u: self.wind_u.clone(),
            wind_v: self.wind_v.clone(),
            accumulator_s: self.accumulator_s,
        }
    }

    pub fn restore(&mut self, snapshot: &Snapshot) -> Result<()> {
        let cells = self.config.grid[0] as usize * self.config.grid[1] as usize;
        if snapshot.columns.len() != cells
            || snapshot.wind_u.len() != cells
            || snapshot.wind_v.len() != cells
            || !snapshot.time_s.is_finite()
            || !snapshot.accumulator_s.is_finite()
        {
            return Err("snapshot does not match the weather grid".into());
        }
        self.config = snapshot.config.clone();
        self.time_s = snapshot.time_s;
        self.sun_override = snapshot.sun_override;
        self.director = snapshot.director.clone();
        self.columns = snapshot.columns.clone();
        self.wind_u = snapshot.wind_u.clone();
        self.wind_v = snapshot.wind_v.clone();
        self.accumulator_s = snapshot.accumulator_s;
        Ok(())
    }
}

fn column_from_target(
    config: &Config,
    target: Target,
    sun: SunPosition,
    n1: f32,
    n2: f32,
    system: f32,
) -> Column {
    let base = config.climate.base_temperature_c()
        + (config.season - 0.5) * 14.0
        + (sun.elevation_deg as f32 / 90.0).max(-0.2) * 3.0;
    let temperature = base + target.temperature_offset_c + (n1 - 0.5) * 1.5;
    let cloud_fraction = (target.cloud_cover * (0.75 + 0.4 * n1) + system * 0.25).clamp(0.0, 1.0);
    let wind_dir = crate::kind::mix_angle(target.wind_dir_deg, 360.0 * n2, 0.25);
    let wind_speed = target.wind_m_s * (0.8 + 0.35 * n1);
    let (u, v) = wind_components(wind_dir, wind_speed);
    let precip = target.precipitation_mm_h * (0.5 + 0.8 * system.max(0.2));
    Column {
        temperature_c: temperature,
        humidity: (target.humidity + (n2 - 0.5) * 0.05).clamp(0.02, 1.0),
        cloud_water: (target.cloud_water * (0.75 + 0.4 * n1)).clamp(0.0, 2.2e-4),
        cloud_fraction,
        cloud_base_m: target.cloud_base_m + (n2 - 0.5) * 120.0,
        cloud_top_m: (target.cloud_top_m + (n1 - 0.5) * 300.0).max(target.cloud_base_m + 30.0),
        reff_m: target.reff_m,
        ice_fraction: target.ice_fraction,
        precip_mm_h: precip,
        precip_kind: if precip > 0.02 && cloud_fraction > 0.02 {
            target.precipitation
        } else {
            PrecipitationKind::None
        },
        snow_fraction: target.snow_fraction,
        hail_fraction: target.hail_fraction,
        fog_density: target.fog * (0.6 + 0.7 * n1),
        dust_density: target.dust * (0.6 + 0.7 * n2),
        visibility_m: target.visibility_m,
        wind_u_m_s: u,
        wind_v_m_s: v,
        wind_speed_m_s: wind_speed,
        wind_dir_deg: wind_dir,
        gust_m_s: wind_speed * target.gust_factor,
        lightning_rate_per_min: target.lightning_rate_per_min * (0.4 + system),
        rainbow: target.rainbow,
        cloud_darkening: target.darkening,
        profile: target.profile,
    }
}

impl Target {
    fn temperature_c(self, base_temperature: f32) -> f32 {
        base_temperature + self.temperature_offset_c
    }
}

fn wind_components(dir_deg: f32, speed_m_s: f32) -> (f32, f32) {
    // Weather convention: direction is where the wind comes from. The cloud
    // volume uses +x/+z as its axes, so the advection vector is downwind.
    let angle = (dir_deg + 180.0).to_radians();
    (angle.sin() * speed_m_s, angle.cos() * speed_m_s)
}

fn relax(current: f32, target: f32, rate: f32, dt_s: f32) -> f32 {
    let alpha = 1.0 - (-rate * dt_s).exp();
    current + (target - current) * alpha
}

fn advect_scalar(
    columns: &[Column],
    nx: usize,
    ny: usize,
    dx: f32,
    dt_s: f32,
    u: &[f32],
    v: &[f32],
    field: impl Fn(&Column) -> f32,
) -> Vec<f32> {
    let mut source: Vec<f32> = columns.iter().map(field).collect();
    let mut temp = vec![0.0; source.len()];
    for y in 0..ny {
        for x in 0..nx {
            let i = y * nx + x;
            let cfl = (u[i] * dt_s / dx).clamp(-0.8, 0.8);
            let prev = if cfl >= 0.0 {
                y * nx + (x + nx - 1) % nx
            } else {
                y * nx + (x + 1) % nx
            };
            temp[i] = source[i] - cfl.abs() * (source[i] - source[prev]);
        }
    }
    std::mem::swap(&mut source, &mut temp);
    for y in 0..ny {
        for x in 0..nx {
            let i = y * nx + x;
            let cfl = (v[i] * dt_s / dx).clamp(-0.8, 0.8);
            let prev = if cfl >= 0.0 {
                ((y + ny - 1) % ny) * nx + x
            } else {
                ((y + 1) % ny) * nx + x
            };
            temp[i] = source[i] - cfl.abs() * (source[i] - source[prev]);
        }
    }
    temp
}

fn hash01(seed: u64, x: u32, y: u32) -> f32 {
    let mut h = seed
        ^ u64::from(x).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ u64::from(y).wrapping_mul(0xD1B5_4A32_D192_ED03);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    (h >> 40) as f32 / (1u32 << 24) as f32
}

pub(crate) fn column_air_density(_column: &Column) -> f64 {
    RHO_AIR
}

fn lightning_pulse(time_s: f64, rate_per_min: f64, seed: u64) -> (f32, u32) {
    if !rate_per_min.is_finite() || rate_per_min <= 0.0 {
        return (0.0, 0);
    }
    let period = (60.0 / rate_per_min).max(0.5);
    let phase = (time_s / period + (seed % 1000) as f64 * 0.137).fract();
    let flash = if phase < 0.12 {
        ((1.0 - phase / 0.12) * 0.95) as f32
    } else {
        0.0
    };
    let bolt_seed = (time_s / period).floor() as u32 ^ (seed as u32).rotate_left(7);
    (flash, bolt_seed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Climate, Config, WeatherKind};

    fn make_model(kind: WeatherKind) -> Model {
        let mut config = Config::fixed(kind);
        config.grid = [32, 32];
        config.volume_dims = [16, 12, 16];
        Model::new(config).unwrap()
    }

    #[test]
    fn fixed_states_are_reachable_and_finite() {
        for kind in [
            WeatherKind::Clear,
            WeatherKind::Rain,
            WeatherKind::Thunderstorm,
            WeatherKind::Blizzard,
            WeatherKind::Fog,
            WeatherKind::Dust,
        ] {
            let mut model = make_model(kind);
            model.advance(60.0).unwrap();
            let d = model.diagnostics();
            assert!(d.temperature_c.is_finite());
            assert!(d.cloud_cover.is_finite());
            assert!(d.visibility_m.is_finite());
            assert!(d.precipitation.rate_mm_h >= 0.0);
        }
    }

    #[test]
    fn clear_has_no_precipitation() {
        let mut model = make_model(WeatherKind::Clear);
        model.advance(120.0).unwrap();
        assert_eq!(
            model.diagnostics().precipitation.kind,
            PrecipitationKind::None
        );
    }

    /// The asynchronous volume rebuild clones the model and samples the clone on
    /// a worker thread. That is only equivalent to sampling the live model if a
    /// clone is byte-identical, so pin it down here.
    #[test]
    fn cloned_model_samples_byte_identical_volume() {
        let mut model = make_model(WeatherKind::Rain);
        model.advance(90.0).unwrap();
        let snapshot = model.clone();

        let source = model.sample_volume().unwrap();
        let cloned = snapshot.sample_volume().unwrap();
        assert_eq!(source.dims, cloned.dims);
        assert_eq!(source.origin_m, cloned.origin_m);
        assert_eq!(source.spacing_m, cloned.spacing_m);
        assert_eq!(source.data.len(), cloned.data.len());
        let source_bits: Vec<u32> = source.data.iter().map(|value| value.to_bits()).collect();
        let cloned_bits: Vec<u32> = cloned.data.iter().map(|value| value.to_bits()).collect();
        assert_eq!(source_bits, cloned_bits, "cloned model volume differs");

        // The clone is an immutable snapshot: advancing the live model afterwards
        // must not change what the clone samples.
        model.advance(30.0).unwrap();
        let cloned_later = snapshot.sample_volume().unwrap();
        assert_eq!(cloned.data, cloned_later.data, "snapshot was not immutable");
    }

    #[test]
    fn snapshot_round_trips() {
        let mut model = make_model(WeatherKind::Rain);
        model.advance(30.0).unwrap();
        let before = model.diagnostics();
        let snapshot = model.snapshot();
        let mut fresh = make_model(WeatherKind::Rain);
        fresh.restore(&snapshot).unwrap();
        let after = fresh.diagnostics();
        assert!((before.temperature_c - after.temperature_c).abs() < 1.0e-6);
        assert_eq!(before.kind, after.kind);
    }

    #[test]
    fn snow_states_stay_frozen_in_temperate_climate() {
        for kind in [
            WeatherKind::LightSnow,
            WeatherKind::HeavySnow,
            WeatherKind::Blizzard,
            WeatherKind::Sleet,
            WeatherKind::FreezingRain,
        ] {
            let mut model = make_model(kind);
            model.advance(30.0).unwrap();
            let d = model.diagnostics();
            assert!(d.temperature_c < 4.0, "{kind:?}: {d:?}");
            assert!(
                matches!(
                    d.precipitation.kind,
                    PrecipitationKind::Snow
                        | PrecipitationKind::Sleet
                        | PrecipitationKind::FreezingRain
                ),
                "{kind:?}: {d:?}"
            );
        }
    }

    #[test]
    fn hail_is_not_degraded_by_a_warm_surface_layer() {
        let mut model = make_model(WeatherKind::Hailstorm);
        model.advance(30.0).unwrap();
        let d = model.diagnostics();
        assert!(d.temperature_c > 4.0);
        assert_eq!(d.precipitation.kind, PrecipitationKind::Hail);
    }

    #[test]
    fn fixed_step_accumulator_is_frame_rate_independent() {
        let mut a = make_model(WeatherKind::Rain);
        let mut b = make_model(WeatherKind::Rain);
        a.advance(10.0).unwrap();
        for _ in 0..10 {
            b.advance(1.0).unwrap();
        }
        let da = a.diagnostics();
        let db = b.diagnostics();
        assert!((da.time_s - db.time_s).abs() < 1.0e-9);
        assert!((da.temperature_c - db.temperature_c).abs() < 1.0e-5);
        assert!((da.cloud_cover - db.cloud_cover).abs() < 1.0e-5);
    }

    #[test]
    fn auto_mode_changes_weather() {
        let mut config = Config::auto(Climate::Temperate);
        config.grid = [32, 32];
        config.volume_dims = [16, 12, 16];
        let mut model = Model::new(config).unwrap();
        for _ in 0..600 {
            model.advance(1.0).unwrap();
        }
        assert!(model.diagnostics().time_s >= 600.0);
        assert!(model.diagnostics().cloud_cover >= 0.0);
    }
}
