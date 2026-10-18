//! Real-time volumetric cloud transport for the sky-tracer applications.
//!
//! This crate is independent of the atmosphere solvers and of the weather
//! model. It accepts a generic cloud volume texture and generic environment
//! textures; the application owns composition and unit conversion.
//!
//! v1 approximates visible cloud optics as gray, uses a two-lobe HG phase and
//! 2-3 multiple-scattering octaves; there is no Mie/Hu-Stamnes table. The
//! renderer does include sub-grid detail erosion, aerial perspective, a
//! top-down ground cloud-shadow map and a lightweight rotation-only temporal
//! resolve, but each is an approximation documented in
//! `docs/clouds_realtime.md` together with the remaining physical gaps.

use std::error::Error;

use serde::{Deserialize, Serialize};

pub mod optics;
pub mod renderer;

pub use renderer::Renderer;

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    Balanced,
    High,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub quality: Quality,
    pub max_steps: u32,
    /// Volumetric trace resolution as a fraction of the output (0.25..=1.0).
    /// 0.8 is the balanced app tier, 0.85 the high tier.
    pub trace_scale: f32,
    /// Sun optical-depth march steps per lit cloud sample (1..=12). More steps
    /// sharpen self-shadowing at a linear cost in the lighting path.
    pub sun_steps: u32,
    pub ms_octaves: u32,
    pub samples: u32,
    pub volume_dims: [u32; 3],
    pub extinction_scale: f32,
    pub ambient_scale: f32,
    /// Strength of the sub-grid 3D detail-noise erosion in [0, 1]. 0 (the
    /// default) reproduces the pure weather-volume look; values around 0.3-0.6
    /// add billows and holes but are an experimental visual model (see
    /// docs/clouds_realtime.md).
    pub detail_strength: f32,
    /// Aerial-perspective e-folding distance in metres; 0 disables the blend.
    /// A visual approximation that fades distant cloud radiance toward the sky.
    pub aerial_distance_m: f32,
    /// Strength of the projected cloud shadow on the ground in [0, 1].
    /// 0 disables the shadow map; the workbench uses 0.9.
    pub cloud_shadow_strength: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            quality: Quality::Balanced,
            max_steps: 40,
            trace_scale: 1.0,
            sun_steps: 4,
            ms_octaves: 3,
            samples: 2,
            volume_dims: [96, 48, 96],
            extinction_scale: 1.0,
            ambient_scale: 0.35,
            detail_strength: 0.0,
            aerial_distance_m: 0.0,
            cloud_shadow_strength: 0.0,
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.max_steps < 8
            || self.max_steps > 256
            || !(1..=12).contains(&self.sun_steps)
            || !self.trace_scale.is_finite()
            || !(0.25..=1.0).contains(&self.trace_scale)
            || !(1..=4).contains(&self.ms_octaves)
            || !(1..=16).contains(&self.samples)
            || self.volume_dims.iter().any(|&n| !(4..=256).contains(&n))
            || !self.extinction_scale.is_finite()
            || self.extinction_scale < 0.0
            || !self.ambient_scale.is_finite()
            || self.ambient_scale < 0.0
            || !self.detail_strength.is_finite()
            || !(0.0..=1.0).contains(&self.detail_strength)
            || !self.aerial_distance_m.is_finite()
            || self.aerial_distance_m < 0.0
            || !self.cloud_shadow_strength.is_finite()
            || !(0.0..=1.0).contains(&self.cloud_shadow_strength)
        {
            return Err("invalid cloud renderer configuration".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeDescriptor {
    pub dims: [u32; 3],
    pub origin_m: [f32; 3],
    pub spacing_m: [f32; 3],
}

impl VolumeDescriptor {
    pub fn extent_m(&self) -> [f32; 3] {
        [
            self.dims[0] as f32 * self.spacing_m[0],
            self.dims[1] as f32 * self.spacing_m[1],
            self.dims[2] as f32 * self.spacing_m[2],
        ]
    }
}

/// Encode a cloud-volume payload from f32 to the half-precision bit pattern
/// stored in the `Rgba16Float` volume texture.
///
/// This is the CPU-heavy half of the upload (6.3 M conversions for the
/// workbench volume) and is pure data, so callers that reconstruct the volume
/// on a worker thread can encode it there as well and only hand the finished
/// `u16` payload to the render thread. The conversion is exactly the one
/// [`Renderer::upload_volume`] performs, so moving it off-thread does not change
/// any voxel.
pub fn encode_volume_half(data: &[f32]) -> Vec<u16> {
    data.iter()
        .map(|&value| half::f16::from_f32(value).to_bits())
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub origin_m: [f32; 3],
    pub yaw_deg: f32,
    pub pitch_deg: f32,
    pub fov_y_deg: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sun {
    pub direction: [f32; 3],
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub frames: u64,
    pub volume_uploads: u64,
    pub last_steps: u32,
    pub last_output: [u32; 2],
    /// Resolution of the volumetric trace buffer (output scaled by `trace_scale`).
    pub last_trace: [u32; 2],
}
