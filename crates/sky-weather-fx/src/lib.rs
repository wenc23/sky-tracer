//! Behaviour-first, screen-space weather effects for the sky-tracer applications.
//!
//! `sky-weather-fx` is independent of the atmosphere solvers and of the weather
//! model. The application supplies weather intensities, camera and sun, and the
//! crate composites stylised precipitation, haze and lightning over an
//! application-provided sky background. Everything is screen-space: there is no
//! geometry pass, no depth buffer and no particle simulation state.
//!
//! # Threading and timing
//!
//! Every layer is a pure function of the pixel, a per-layer hash and `time_s`.
//! The result is temporally deterministic: the same input produces the same
//! image. Particle motion is implemented by scrolling the procedural fields with
//! `time_s`, not by simulating particles, so no frame-to-frame state is kept.
//!
//! # Screen-space approximations (v1)
//!
//! - Rain/sleet streaks and snow/hail flakes are procedural screen-space layers
//!   with stylised per-layer parallax (near layers are larger, faster and
//!   brighter). They are not occluded by scene geometry and are not attenuated
//!   by scene depth.
//! - Fog/haze, dust, cloud darkening, the rainbow and the lightning bolt are
//!   analytic screen-space terms driven by the view ray. They are not
//!   transported through a participating medium; `cloud_darkening` is a
//!   procedural darkening, not a real shadow.
//! - The rainbow is an additive primary/secondary band around the anti-solar
//!   direction, faded by solar elevation; it is not a spectral dispersion model.
//! - The lightning bolt is a procedural jagged polyline and the flash is the
//!   application-owned envelope (`lightning_flash`); the crate does not simulate
//!   electrical discharges.
//! - Zero weather (all intensities 0, flash 0) copies the background exactly: the
//!   shader short-circuits to a background load and the CPU-side test covers the
//!   compositing identity.
//!
//! # Required texture formats
//!
//! The background view passed to [`Renderer::set_background`] must be a
//! float-sample texture, typically `Rgba32Float` or `Rgba16Float`, because it is
//! read with `textureLoad`. The output target is `Rgba32Float` in linear
//! Rec.2020 radiance with the viewport size given to [`Renderer::resize`]. The
//! application remains responsible for exposure and tone mapping.

use std::error::Error;

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

pub mod renderer;

pub use renderer::Renderer;

/// Quality preset used to size the procedural particle layer counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quality {
    /// Cheaper preset: up to 2 rain, 2 snow, 1 hail and 1 sleet layers.
    #[default]
    Low,
    /// Higher quality: up to 3 rain, 3 snow, 2 hail and 2 sleet layers.
    High,
}

/// Renderer configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub quality: Quality,
    /// Upper bound on the number of screen-space particle layers allocated in
    /// the order rain, snow, hail, sleet. Validated to `1..=32`.
    pub max_effects: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            quality: Quality::High,
            max_effects: 10,
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if !(1..=32).contains(&self.max_effects) {
            return Err(format!(
                "weather effect budget must be in 1..=32, got {}",
                self.max_effects
            )
            .into());
        }
        Ok(())
    }

    /// Particle layers allocated for this quality and budget, in the order
    /// rain, snow, hail, sleet.
    pub fn layer_counts(&self) -> [u32; 4] {
        let wanted = match self.quality {
            Quality::Low => [2, 2, 1, 1],
            Quality::High => [3, 3, 2, 2],
        };
        let mut counts = [0; 4];
        let mut budget = self.max_effects;
        for (count, want) in counts.iter_mut().zip(wanted) {
            *count = want.min(budget);
            budget -= *count;
        }
        counts
    }

    /// Number of jagged segments in the procedural lightning bolt.
    pub fn bolt_segments(&self) -> u32 {
        match self.quality {
            Quality::Low => 10,
            Quality::High => 16,
        }
    }
}

/// Camera pose. `origin_m` is the world-space observer position in metres and is
/// used to parallax the procedural cloud shadow and haze patterns.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub origin_m: [f32; 3],
    pub yaw_deg: f32,
    pub pitch_deg: f32,
    pub fov_y_deg: f32,
}

/// Direction toward the sun, not required to be normalized.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sun {
    pub direction: [f32; 3],
}

/// One frame of weather input. Intensity channels are clamped to `[0, 1]` after
/// finite validation; `lightning_flash` is the application-owned flash envelope.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Input {
    pub time_s: f32,
    pub camera: Camera,
    pub sun: Sun,
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

impl Input {
    /// Validate every input value. Non-finite values, a negative `time_s`, an
    /// out-of-range vertical field of view and a zero sun direction are errors.
    /// Intensities outside `[0, 1]` are accepted and clamped during
    /// normalization; they are not silently discarded.
    pub fn validate(&self) -> Result<()> {
        let scalars = [
            self.time_s,
            self.rain,
            self.snow,
            self.hail,
            self.sleet,
            self.fog,
            self.dust,
            self.cloud_darkening,
            self.rainbow,
            self.lightning_flash,
            self.camera.origin_m[0],
            self.camera.origin_m[1],
            self.camera.origin_m[2],
            self.camera.yaw_deg,
            self.camera.pitch_deg,
            self.camera.fov_y_deg,
            self.sun.direction[0],
            self.sun.direction[1],
            self.sun.direction[2],
            self.wind[0],
            self.wind[1],
        ];
        if scalars.iter().any(|value| !value.is_finite()) {
            return Err("weather input contains a non-finite value".into());
        }
        if self.time_s < 0.0 {
            return Err("weather time_s must be nonnegative".into());
        }
        if !(1.0..170.0).contains(&self.camera.fov_y_deg) {
            return Err("camera vertical field of view must be in (1, 170) degrees".into());
        }
        let sun = self.sun.direction;
        let sun_length_squared = sun[0] * sun[0] + sun[1] * sun[1] + sun[2] * sun[2];
        if sun_length_squared <= 1.0e-12 {
            return Err("sun direction must be a nonzero finite vector".into());
        }
        Ok(())
    }

    /// True when every weather channel clamps to zero. In that case `render`
    /// copies the background exactly; applications can use this to skip effect
    /// updates while still presenting the background.
    pub fn is_zero_effect(&self) -> bool {
        self.normalize()
            .map(|normalized| normalized.all_zero())
            .unwrap_or(false)
    }

    /// Validated and normalized shader inputs.
    pub(crate) fn normalize(&self) -> Result<NormalizedInput> {
        self.validate()?;
        let sun = self.sun.direction;
        let inverse_length = 1.0 / (sun[0] * sun[0] + sun[1] * sun[1] + sun[2] * sun[2]).sqrt();
        let (forward, right, up) = camera_basis(self.camera.yaw_deg, self.camera.pitch_deg);
        let clamp_unit = |value: f32| value.clamp(0.0, 1.0);
        Ok(NormalizedInput {
            time_s: self.time_s,
            origin_m: self.camera.origin_m,
            forward,
            right,
            up,
            sun_direction: [
                sun[0] * inverse_length,
                sun[1] * inverse_length,
                sun[2] * inverse_length,
            ],
            intensities: [
                clamp_unit(self.rain),
                clamp_unit(self.snow),
                clamp_unit(self.hail),
                clamp_unit(self.sleet),
            ],
            atmosphere: [
                clamp_unit(self.fog),
                clamp_unit(self.dust),
                clamp_unit(self.cloud_darkening),
                clamp_unit(self.rainbow),
            ],
            wind: self.wind,
            lightning_flash: clamp_unit(self.lightning_flash),
            lightning_seed: self.lightning_seed,
        })
    }
}

/// Shader-ready input after validation and clamping.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NormalizedInput {
    pub time_s: f32,
    pub origin_m: [f32; 3],
    pub forward: [f32; 3],
    pub right: [f32; 3],
    pub up: [f32; 3],
    pub sun_direction: [f32; 3],
    /// rain, snow, hail, sleet.
    pub intensities: [f32; 4],
    /// fog, dust, cloud darkening, rainbow.
    pub atmosphere: [f32; 4],
    pub wind: [f32; 2],
    pub lightning_flash: f32,
    pub lightning_seed: u32,
}

impl NormalizedInput {
    /// True when the shader's exact zero-weather short-circuit is taken.
    pub fn all_zero(&self) -> bool {
        self.intensities.iter().all(|value| *value == 0.0)
            && self.atmosphere.iter().all(|value| *value == 0.0)
            && self.lightning_flash == 0.0
    }
}

/// Render statistics.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub frames: u64,
    pub last_output: [u32; 2],
}

/// Camera basis matching the conventions shared by the sky renderers:
/// `forward=(sin(yaw)cos(pitch), sin(pitch), cos(yaw)cos(pitch))`,
/// `right=(cos(yaw), 0, -sin(yaw))`, `up=cross(forward, right)`.
pub fn camera_basis(yaw_deg: f32, pitch_deg: f32) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let yaw = yaw_deg.to_radians();
    let pitch = pitch_deg.to_radians();
    let forward = [
        yaw.sin() * pitch.cos(),
        pitch.sin(),
        yaw.cos() * pitch.cos(),
    ];
    let right = [yaw.cos(), 0.0, -yaw.sin()];
    let up = [
        forward[1] * right[2] - forward[2] * right[1],
        forward[2] * right[0] - forward[0] * right[2],
        forward[0] * right[1] - forward[1] * right[0],
    ];
    (forward, right, up)
}

/// CPU mirror of the shader's particle blend; an opacity of zero returns the
/// input colour bit-for-bit, which is the zero-weather compositing identity.
pub fn blend(background: [f32; 3], overlay: [f32; 3], opacity: f32) -> [f32; 3] {
    [
        background[0] + (overlay[0] - background[0]) * opacity,
        background[1] + (overlay[1] - background[1]) * opacity,
        background[2] + (overlay[2] - background[2]) * opacity,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_camera() -> Camera {
        Camera {
            origin_m: [120.0, 45.0, -80.0],
            yaw_deg: 37.0,
            pitch_deg: 8.0,
            fov_y_deg: 60.0,
        }
    }

    fn sample_sun() -> Sun {
        Sun {
            direction: [0.4, 0.6, 0.2],
        }
    }

    fn zero_weather_input() -> Input {
        Input {
            time_s: 0.0,
            camera: sample_camera(),
            sun: sample_sun(),
            rain: 0.0,
            snow: 0.0,
            hail: 0.0,
            sleet: 0.0,
            fog: 0.0,
            dust: 0.0,
            wind: [3.0, -1.0],
            cloud_darkening: 0.0,
            rainbow: 0.0,
            lightning_flash: 0.0,
            lightning_seed: 7,
        }
    }

    #[test]
    fn default_config_is_valid() {
        let config = Config::default();
        assert_eq!(config.validate().is_ok(), true);
        assert_eq!(config.layer_counts().iter().sum::<u32>(), 10);
    }

    #[test]
    fn config_validation_rejects_zero_and_large_budgets() {
        for budget in [0, 33, u32::MAX] {
            let config = Config {
                max_effects: budget,
                ..Config::default()
            };
            assert!(config.validate().is_err(), "budget {budget}");
        }
        for budget in [1, 10, 32] {
            let config = Config {
                max_effects: budget,
                ..Config::default()
            };
            assert!(config.validate().is_ok(), "budget {budget}");
        }
    }

    #[test]
    fn layer_budget_is_respected_in_priority_order() {
        let config = Config {
            quality: Quality::High,
            max_effects: 4,
        };
        assert_eq!(config.layer_counts(), [3, 1, 0, 0]);
        let low = Config {
            quality: Quality::Low,
            max_effects: 3,
        };
        assert_eq!(low.layer_counts(), [2, 1, 0, 0]);
        let untouched = Config {
            quality: Quality::Low,
            max_effects: 32,
        };
        assert_eq!(untouched.layer_counts(), [2, 2, 1, 1]);
        for quality in [Quality::Low, Quality::High] {
            for budget in 1..=32 {
                let config = Config {
                    quality,
                    max_effects: budget,
                };
                assert!(config.layer_counts().iter().sum::<u32>() <= budget);
            }
        }
    }

    #[test]
    fn quality_selects_bolt_detail() {
        assert!(
            Config {
                quality: Quality::High,
                ..Config::default()
            }
            .bolt_segments()
                > Config {
                    quality: Quality::Low,
                    ..Config::default()
                }
                .bolt_segments()
        );
    }

    #[test]
    fn zero_weather_is_valid_and_classified_as_zero() {
        let input = zero_weather_input();
        input.validate().unwrap();
        let normalized = input.normalize().unwrap();
        assert!(normalized.all_zero());
        assert!(input.is_zero_effect());
    }

    #[test]
    fn nonzero_weather_is_not_classified_as_zero() {
        for field in 0..8 {
            let mut input = zero_weather_input();
            match field {
                0 => input.rain = 0.1,
                1 => input.snow = 0.1,
                2 => input.hail = 0.1,
                3 => input.sleet = 0.1,
                4 => input.fog = 0.1,
                5 => input.dust = 0.1,
                6 => input.cloud_darkening = 0.1,
                7 => input.rainbow = 0.1,
                _ => unreachable!(),
            }
            assert!(!input.normalize().unwrap().all_zero(), "field {field}");
        }
        let mut flash = zero_weather_input();
        flash.lightning_flash = 0.1;
        assert!(!flash.normalize().unwrap().all_zero());
    }

    #[test]
    fn validation_rejects_non_finite_and_out_of_range_values() {
        let mut input = zero_weather_input();
        input.rain = f32::NAN;
        assert!(input.validate().is_err());
        input = zero_weather_input();
        input.wind[1] = f32::INFINITY;
        assert!(input.validate().is_err());
        input = zero_weather_input();
        input.time_s = -1.0;
        assert!(input.validate().is_err());
        input = zero_weather_input();
        input.camera.fov_y_deg = 0.5;
        assert!(input.validate().is_err());
        input = zero_weather_input();
        input.camera.origin_m[2] = f32::NEG_INFINITY;
        assert!(input.validate().is_err());
        input = zero_weather_input();
        input.sun.direction = [0.0, 0.0, 0.0];
        assert!(input.validate().is_err());
        input = zero_weather_input();
        input.sun.direction = [f32::NAN, 1.0, 0.0];
        assert!(input.validate().is_err());
    }

    #[test]
    fn negative_intensities_clamp_to_zero_effect() {
        let mut input = zero_weather_input();
        input.rain = -3.0;
        input.fog = -0.5;
        input.lightning_flash = -1.0;
        assert!(input.validate().is_ok());
        assert!(input.is_zero_effect());
    }

    #[test]
    fn intensities_are_clamped_to_unit_range() {
        let mut input = zero_weather_input();
        input.rain = 4.0;
        input.snow = -2.0;
        input.hail = 0.5;
        input.sleet = 1.0;
        input.fog = 3.0;
        input.dust = -1.0;
        input.cloud_darkening = 2.0;
        input.rainbow = 0.25;
        input.lightning_flash = 9.0;
        let normalized = input.normalize().unwrap();
        assert_eq!(normalized.intensities, [1.0, 0.0, 0.5, 1.0]);
        assert_eq!(normalized.atmosphere, [1.0, 0.0, 1.0, 0.25]);
        assert_eq!(normalized.lightning_flash, 1.0);
    }

    #[test]
    fn camera_basis_is_orthonormal_and_matches_documented_formula() {
        for (yaw, pitch) in [
            (0.0f32, 0.0f32),
            (90.0, 0.0),
            (37.0, 15.0),
            (180.0, -30.0),
            (300.0, 60.0),
        ] {
            let (forward, right, up) = camera_basis(yaw, pitch);
            let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
            assert!((dot(forward, forward) - 1.0).abs() < 1.0e-5);
            assert!((dot(right, right) - 1.0).abs() < 1.0e-5);
            assert!((dot(up, up) - 1.0).abs() < 1.0e-5);
            assert!(dot(forward, right).abs() < 1.0e-5);
            assert!(dot(forward, up).abs() < 1.0e-5);
            assert!(dot(right, up).abs() < 1.0e-5);
        }
        let (forward, right, up) = camera_basis(0.0, 0.0);
        assert!((forward[2] - 1.0).abs() < 1.0e-6);
        assert!((right[0] - 1.0).abs() < 1.0e-6);
        assert!((up[1] - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn zero_opacity_blend_is_bit_exact_background() {
        let background = [0.25, 0.5, 1.0];
        let overlay = [0.9, 0.1, 0.3];
        let output = blend(background, overlay, 0.0);
        for channel in 0..3 {
            assert_eq!(output[channel].to_bits(), background[channel].to_bits());
        }
    }
}
