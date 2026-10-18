//! Cloud-volume reconstruction from the 2D behaviour field.
//!
//! The public volume contract is unchanged: `[LWC, r_eff, ice_fraction,
//! cloud_fraction]`, x fastest. The horizontal structure is a multi-scale
//! hash field advected by the reported wind; it is a visual reconstruction, not
//! resolved turbulence. Every octave is band-limited to the reconstruction grid
//! (>= ~4 voxels per wavelength, 250 m horizontal / 146 m vertical) so the
//! thresholded cloud boundary does not alias into a voxel lattice; the sub-voxel
//! billows are supplied by the renderer's detail-noise erosion instead (see
//! `docs/clouds_realtime.md`).

use crate::{CloudVolume, Model, Result, kind::CloudProfile, model};

fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0).max(1.0e-9)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn hash3(seed: u64, x: i32, y: i32, z: i32) -> f64 {
    let mut h = seed
        ^ (x as u32 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u32 as u64).wrapping_mul(0xD1B5_4A32_D192_ED03)
        ^ (z as u32 as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    (h >> 11) as f64 / (1u64 << 53) as f64
}

fn value_noise3(seed: u64, x: f64, y: f64, z: f64) -> f64 {
    let xi = x.floor() as i32;
    let yi = y.floor() as i32;
    let zi = z.floor() as i32;
    let fx = x - x.floor();
    let fy = y - y.floor();
    let fz = z - z.floor();
    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sy = fy * fy * (3.0 - 2.0 * fy);
    let sz = fz * fz * (3.0 - 2.0 * fz);
    let mut v = 0.0;
    for dz in 0..2 {
        for dy in 0..2 {
            for dx in 0..2 {
                let w = (if dx == 0 { 1.0 - sx } else { sx })
                    * (if dy == 0 { 1.0 - sy } else { sy })
                    * (if dz == 0 { 1.0 - sz } else { sz });
                v += w * hash3(seed, xi + dx, yi + dy, zi + dz);
            }
        }
    }
    v
}

/// Band-limited cumulus field.
///
/// Every octave here is at least ~4 reconstruction voxels per wavelength
/// horizontally (250 m voxels) and vertically (146 m layers). Earlier versions
/// added 750 m and 520 m octaves; at 2-3 samples per wavelength those octaves
/// alias into a visible blocky lattice once the field is thresholded, which is
/// exactly the "Minecraft cloud" silhouette seen on distant cumulus. The
/// sub-voxel billows are instead left to the renderer's detail-noise erosion,
/// which has no reconstruction limit.
///
/// A very slow vertical morph keeps the field "boiling" under a fixed weather
/// state; horizontal advection is applied by the caller from the reported wind.
fn cloud_noise(seed: u64, x: f64, y: f64, z: f64, time_s: f64) -> f64 {
    let t = time_s / 900.0;
    let n0 = value_noise3(seed, x / 4_200.0, y / 1_700.0 + t * 0.7, z / 4_200.0);
    let n1 = value_noise3(
        seed ^ 0x517C_C1B7_2722_0A95,
        x / 1_500.0,
        y / 800.0,
        z / 1_500.0,
    );
    let n2 = value_noise3(
        seed ^ 0x2A9F_31D4_6C07_B5E3,
        x / 2_600.0,
        y / 1_100.0,
        z / 2_600.0,
    );
    0.55 * n0 + 0.27 * n1 + 0.18 * n2
}

#[derive(Clone, Copy)]
struct SampledColumn {
    q_c: f64,
    cloud_fraction: f64,
    base: f64,
    top: f64,
    reff: f64,
    ice: f64,
    air_density: f64,
    profile: CloudProfile,
}

fn sample_column(
    columns: &[model::Column],
    nx: usize,
    ny: usize,
    gx: f64,
    gy: f64,
) -> SampledColumn {
    let x0 = gx.floor() as i64;
    let y0 = gy.floor() as i64;
    let tx = gx - gx.floor();
    let ty = gy - gy.floor();
    let x1 = (x0 + 1).rem_euclid(nx as i64) as usize;
    let y1 = (y0 + 1).rem_euclid(ny as i64) as usize;
    let x0 = x0.rem_euclid(nx as i64) as usize;
    let y0 = y0.rem_euclid(ny as i64) as usize;
    let c00 = &columns[y0 * nx + x0];
    let c10 = &columns[y0 * nx + x1];
    let c01 = &columns[y1 * nx + x0];
    let c11 = &columns[y1 * nx + x1];
    let lerp = |a: f32, b: f32, c: f32, d: f32| -> f64 {
        let a = f64::from(a);
        let b = f64::from(b);
        let c = f64::from(c);
        let d = f64::from(d);
        (a * (1.0 - tx) + b * tx) * (1.0 - ty) + (c * (1.0 - tx) + d * tx) * ty
    };
    SampledColumn {
        q_c: lerp(
            c00.cloud_water,
            c10.cloud_water,
            c01.cloud_water,
            c11.cloud_water,
        )
        .max(0.0),
        cloud_fraction: lerp(
            c00.cloud_fraction,
            c10.cloud_fraction,
            c01.cloud_fraction,
            c11.cloud_fraction,
        )
        .clamp(0.0, 1.0),
        base: lerp(
            c00.cloud_base_m,
            c10.cloud_base_m,
            c01.cloud_base_m,
            c11.cloud_base_m,
        ),
        top: lerp(
            c00.cloud_top_m,
            c10.cloud_top_m,
            c01.cloud_top_m,
            c11.cloud_top_m,
        ),
        reff: lerp(c00.reff_m, c10.reff_m, c01.reff_m, c11.reff_m).clamp(1.0e-6, 80.0e-6),
        ice: lerp(
            c00.ice_fraction,
            c10.ice_fraction,
            c01.ice_fraction,
            c11.ice_fraction,
        )
        .clamp(0.0, 1.0),
        air_density: lerp(
            model::column_air_density(c00) as f32,
            model::column_air_density(c10) as f32,
            model::column_air_density(c01) as f32,
            model::column_air_density(c11) as f32,
        ),
        profile: if ty < 0.5 { c00.profile } else { c01.profile },
    }
}

fn profile_shape(profile: CloudProfile, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match profile {
        CloudProfile::None => 0.0,
        CloudProfile::Cumulus => (std::f64::consts::PI * t).sin().powf(0.7).max(0.04),
        CloudProfile::Stratus => 0.55 + 0.45 * t,
        CloudProfile::Deep => 0.65 + 0.35 * (std::f64::consts::PI * t).sin(),
        CloudProfile::Snow => 0.45 + 0.55 * (std::f64::consts::PI * t).sin().powf(0.6),
        CloudProfile::Cirrus => (-((t - 0.5) / 0.28).powi(2)).exp() * (0.6 + 0.4 * t),
        CloudProfile::Fog => (1.0 - t).powf(1.5),
    }
}

/// Shared read-only inputs for one volume slice.
#[derive(Clone, Copy)]
struct VolumeContext<'a> {
    model: &'a Model,
    nx: usize,
    ny: usize,
    origin: [f64; 3],
    spacing: [f64; 3],
    extent: [f64; 3],
    gx_n: usize,
    gy_n: usize,
    dx: f64,
    seed: u64,
    time: f64,
}

impl VolumeContext<'_> {
    /// Fill and LWP-normalise the contiguous z-slice `z0..z1`.
    fn fill(&self, z0: usize, z1: usize, data: &mut [f32], target_lwp: &mut [f64]) {
        let VolumeContext {
            model,
            nx,
            ny,
            origin,
            spacing,
            extent,
            gx_n,
            gy_n,
            dx,
            seed,
            time,
            ..
        } = *self;
        let local_nz = z1 - z0;
        debug_assert_eq!(data.len(), nx * ny * local_nz * 4);
        debug_assert_eq!(target_lwp.len(), nx * local_nz);
        for local_iz in 0..local_nz {
            let iz = z0 + local_iz;
            let z = origin[2] + (iz as f64 + 0.5) * spacing[2];
            for ix in 0..nx {
                let x = origin[0] + (ix as f64 + 0.5) * spacing[0];
                let gx = ((x + extent[0] * 0.5) / dx).rem_euclid(gx_n as f64);
                let gy = ((z + extent[2] * 0.5) / dx).rem_euclid(gy_n as f64);
                let column = sample_column(&model.columns, gx_n, gy_n, gx, gy);
                let wind_u =
                    model.wind_u[(gy as usize).min(gy_n - 1) * gx_n + (gx as usize).min(gx_n - 1)];
                let wind_v =
                    model.wind_v[(gy as usize).min(gy_n - 1) * gx_n + (gx as usize).min(gx_n - 1)];
                // Lumpy cloud tops/bases: the 2D column field gives the broad
                // profile, a low-frequency 3D hash breaks up flat horizontal
                // lids so cumulus silhouettes are not pancakes. The LWP
                // normalisation below still preserves the column water.
                let lump =
                    value_noise3(seed ^ 0x1D2E_3F40_5A6B_7C8D, x / 1_800.0, z / 1_800.0, 0.37);
                let base = column.base + (lump - 0.5) * 200.0;
                let top = (column.top + (lump - 0.5) * 1100.0).max(base + 80.0);
                let depth = (top - base).max(0.0);
                let lwp = column.air_density * column.q_c * depth * column.cloud_fraction;
                target_lwp[local_iz * nx + ix] = lwp;
                if lwp <= 1.0e-9 || column.cloud_fraction <= 1.0e-4 || depth < 20.0 {
                    continue;
                }
                // Wind shear: displace higher parts of the column along the
                // wind direction so towers lean instead of standing as
                // perfectly vertical curtains. Exaggerated (12% of height).
                let speed = (f64::from(wind_u) * f64::from(wind_u)
                    + f64::from(wind_v) * f64::from(wind_v))
                .sqrt()
                .max(0.1);
                let shear_x = f64::from(wind_u) / speed * 0.22;
                let shear_z = f64::from(wind_v) / speed * 0.22;
                // `wind_u`/`wind_v` are the downwind advection components
                // (see `model::wind_components`), so subtracting them from the
                // sampling position makes the cloud pattern drift downwind.
                // The renderer interpolates this same drift between volume
                // uploads so the observed motion is continuous at frame rate
                // (see `Renderer::set_volume_drift`).
                for iy in 0..ny {
                    let y = origin[1] + (iy as f64 + 0.5) * spacing[1];
                    if y <= base || y >= top {
                        continue;
                    }
                    let t = (y - base) / depth;
                    let shape = profile_shape(column.profile, t);
                    if shape <= 0.0 {
                        continue;
                    }
                    let shear = y - base;
                    let n = cloud_noise(
                        seed,
                        x - f64::from(wind_u) * time + shear_x * shear,
                        y,
                        z - f64::from(wind_v) * time + shear_z * shear,
                        time,
                    );
                    let c = column.cloud_fraction;
                    // High coverage decks are nearly solid overcast rather than
                    // a sparse scatter of puffy cells: shrink the noise
                    // threshold as coverage approaches 1. A narrow transition
                    // keeps cumulus boundaries distinct instead of filling the
                    // whole column with a low-density veil (the LWP
                    // normalisation below keeps the total water unchanged).
                    let edge = 0.035 - 0.025 * smoothstep(0.75, 0.95, c);
                    let mask = smoothstep(1.0 - c - edge, 1.0 - c + edge, n);
                    let lwc = column.air_density * column.q_c * shape * mask;
                    let index = ((ix + nx * (iy + ny * local_iz)) * 4) as usize;
                    data[index] = lwc.max(0.0) as f32;
                    data[index + 1] = column.reff as f32;
                    data[index + 2] = column.ice as f32;
                    data[index + 3] = c as f32;
                }
            }
        }

        for local_iz in 0..local_nz {
            for ix in 0..nx {
                let target = target_lwp[local_iz * nx + ix];
                let mut actual = 0.0;
                for iy in 0..ny {
                    let index = ((ix + nx * (iy + ny * local_iz)) * 4) as usize;
                    actual += f64::from(data[index]) * spacing[1];
                }
                if actual > 1.0e-12 {
                    let scale = (target / actual) as f32;
                    for iy in 0..ny {
                        let index = ((ix + nx * (iy + ny * local_iz)) * 4) as usize;
                        data[index] = (data[index] * scale).max(0.0);
                    }
                }
            }
        }
    }
}

/// Worker count for volume generation. Small volumes stay single-threaded;
/// large ones fan out over z-slices, which are independent because the LWP
/// normalisation is per column.
fn volume_workers(override_count: Option<usize>, nz: usize, voxel_count: usize) -> usize {
    if voxel_count < 100_000 {
        return 1;
    }
    let requested = override_count.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    });
    requested.clamp(1, 8).min(nz.max(1))
}

pub fn sample(model: &Model) -> Result<CloudVolume> {
    sample_with_workers(model, None)
}

fn sample_with_workers(model: &Model, workers: Option<usize>) -> Result<CloudVolume> {
    let config = &model.config;
    let dims = config.volume_dims;
    let [nx, ny, nz] = [dims[0] as usize, dims[1] as usize, dims[2] as usize];
    if nx == 0 || ny == 0 || nz == 0 {
        return Err("cloud volume dimensions must be positive".into());
    }
    let extent = config.volume_extent_m;
    let origin = [-extent[0] * 0.5, 0.0, -extent[2] * 0.5];
    let spacing = [
        extent[0] / nx as f64,
        extent[1] / ny as f64,
        extent[2] / nz as f64,
    ];
    let gx_n = config.grid[0] as usize;
    let gy_n = config.grid[1] as usize;
    let dx = config.dx_m;
    let mut data = vec![0.0f32; nx * ny * nz * 4];
    let mut target_lwp = vec![0.0f64; nx * nz];
    let context = VolumeContext {
        model,
        nx,
        ny,
        origin,
        spacing,
        extent,
        gx_n,
        gy_n,
        dx,
        seed: config.seed,
        time: model.time_s,
    };
    let workers = volume_workers(workers, nz, nx * ny * nz);
    if workers <= 1 {
        context.fill(0, nz, &mut data, &mut target_lwp);
    } else {
        let chunk = nz.div_ceil(workers);
        std::thread::scope(|scope| {
            let mut z0 = 0;
            let chunks = data
                .chunks_mut(chunk * nx * ny * 4)
                .zip(target_lwp.chunks_mut(chunk * nx));
            for (data_chunk, target_chunk) in chunks {
                let z1 = (z0 + chunk).min(nz);
                let start = z0;
                z0 = z1;
                scope.spawn(move || context.fill(start, z1, data_chunk, target_chunk));
            }
        });
    }

    let volume = CloudVolume {
        dims,
        origin_m: [origin[0] as f32, origin[1] as f32, origin[2] as f32],
        spacing_m: [spacing[0] as f32, spacing[1] as f32, spacing[2] as f32],
        data,
    };
    volume.validate()?;
    Ok(volume)
}

#[cfg(test)]
mod tests {
    use super::sample_with_workers;
    use crate::{Config, Model, WeatherKind};

    #[test]
    fn volume_matches_requested_dims_and_is_finite() {
        let mut config = Config::fixed(WeatherKind::Rain);
        config.grid = [16, 16];
        config.volume_dims = [16, 12, 16];
        config.volume_extent_m = [16_000.0, 8_000.0, 16_000.0];
        let mut model = Model::new(config).unwrap();
        model.advance(60.0).unwrap();
        let volume = model.sample_volume().unwrap();
        volume.validate().unwrap();
        assert_eq!(volume.data.len(), 16 * 12 * 16 * 4);
        assert!(
            volume
                .data
                .chunks_exact(4)
                .all(|v| v[0].is_finite() && v[0] >= 0.0)
        );
    }

    #[test]
    fn clear_volume_is_empty() {
        let mut config = Config::fixed(WeatherKind::Clear);
        config.grid = [16, 16];
        config.volume_dims = [8, 8, 8];
        let model = Model::new(config).unwrap();
        let volume = model.sample_volume().unwrap();
        assert!(volume.data.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn app_sized_rain_volume_has_cloud_water() {
        let mut config = Config::fixed(WeatherKind::Rain);
        config.grid = [128, 128];
        config.dx_m = 250.0;
        config.volume_dims = [96, 64, 96];
        let mut model = Model::new(config).unwrap();
        model.advance(120.0).unwrap();
        let volume = model.sample_volume().unwrap();
        let max_lwc = volume
            .data
            .chunks_exact(4)
            .map(|v| v[0])
            .fold(0.0f32, f32::max);
        assert!(max_lwc > 1.0e-5, "max LWC {max_lwc}");
    }

    #[test]
    fn parallel_volume_matches_single_threaded() {
        let mut config = Config::fixed(WeatherKind::PartlyCloudy);
        config.grid = [64, 64];
        config.dx_m = 500.0;
        config.volume_dims = [64, 32, 64];
        let mut model = Model::new(config).unwrap();
        model.advance(60.0).unwrap();
        let single = sample_with_workers(&model, Some(1)).unwrap();
        let parallel = sample_with_workers(&model, Some(8)).unwrap();
        assert_eq!(single.data, parallel.data);
    }
}
