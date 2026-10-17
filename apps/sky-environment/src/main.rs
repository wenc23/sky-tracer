//! Export the realtime atmosphere as a frozen far-field boundary-lighting asset
//! for an external renderer: full-sphere linear-sRGB radiance plus the
//! atmosphere-attenuated direct-sun irradiance.
//!
//! The solver is `sky-realtime`; this application only converts color and units
//! and serialises the result. It introduces no transport of its own, and the
//! output is a frozen boundary (one observer altitude, one sun pose), not a
//! joint solve with the consuming renderer.
//!
//! Output layout (`--out DIR`):
//!   environment.json  provenance, atmosphere inputs, sun metadata, checksums
//!   radiance.f32      rgb32f little-endian, row-major, width*height*3
//!   preview.png       exposure/Reinhard preview for human inspection only
//!
//! Direction mapping matches `sky-realtime`'s `project_environment`:
//!   u = (x + 0.5) / width,  v = (y + 0.5) / height
//!   phi = 2*pi*u,           theta = pi*v
//!   dir = (sin(phi)*sin(theta), cos(theta), cos(phi)*sin(theta))
//! i.e. row 0 is +Y and longitude is atan2(x, z).
//!
//! Usage:
//!   cargo run --release -p sky-environment -- --out out/env_noon --sun-elevation-deg 40
//!   cargo run --release -p sky-environment -- --out out/env_sunset --sun-elevation-deg 2 --sun-azimuth-deg 80

use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use serde_json::json;
use sky_realtime::{Config, Renderer, View, Wavelengths, read_texture};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Export realtime sky as a frozen boundary-lighting asset"
)]
struct Cli {
    /// Output directory; must not already contain an environment.
    #[arg(long, default_value = "out/environment")]
    out: PathBuf,
    #[arg(long, default_value_t = 2048)]
    width: u32,
    #[arg(long, default_value_t = 1024)]
    height: u32,
    #[arg(long, default_value_t = 12.0, allow_hyphen_values = true)]
    sun_elevation_deg: f32,
    #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
    sun_azimuth_deg: f32,
    #[arg(long, default_value_t = 0.2)]
    altitude_km: f32,
    #[arg(long, default_value_t = 1.0)]
    aerosol_scale: f32,
    #[arg(long, default_value_t = 0.18)]
    ground_albedo: f32,
    /// Angular radius of the composited solar disk.
    #[arg(long, default_value_t = 0.265)]
    sun_angular_radius_deg: f32,
    /// Sub-samples per texel when compositing the solar disk below one texel.
    #[arg(long, default_value_t = 4)]
    sun_super_samples: u32,
    #[arg(long, default_value_t = sky_realtime::DEFAULT_RUNTIME_STEPS)]
    steps: u32,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    wavelengths: Option<PathBuf>,
    /// Optional scale applied to radiance and sun irradiance (unit conversion
    /// knob only; consumers are expected to choose exposure themselves).
    #[arg(long, default_value_t = 1.0)]
    exposure_scale: f32,
}

fn main() -> sky_realtime::Result<()> {
    pollster::block_on(run())
}

async fn run() -> sky_realtime::Result<()> {
    let a = Cli::parse();
    if a.width < 2 || a.height < 2 {
        return Err("width and height must both be >= 2".into());
    }
    if !(0.0..90.0).contains(&a.sun_angular_radius_deg) {
        return Err("sun angular radius must be in (0, 90) degrees".into());
    }
    if !a.exposure_scale.is_finite() || a.exposure_scale <= 0.0 {
        return Err("exposure scale must be positive and finite".into());
    }
    std::fs::create_dir_all(&a.out)?;
    for existing in ["environment.json", "radiance.f32", "preview.png"] {
        if a.out.join(existing).exists() {
            return Err(format!(
                "{} already exists; use a new output directory",
                a.out.join(existing).display()
            )
            .into());
        }
    }

    let config: Config = match &a.config {
        Some(path) => serde_json::from_slice(&std::fs::read(path)?)?,
        None => Config::balanced(),
    };
    let wavelengths: Wavelengths = match &a.wavelengths {
        Some(path) => serde_json::from_slice(&std::fs::read(path)?)?,
        None => Wavelengths::optimized_four(),
    };
    let model = sky_realtime::model::Model::earth_with_aerosol_scale(a.aerosol_scale)?;

    // ---- GPU: solve the fixed atmosphere once, then project the environment ----
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        })
        .await?;
    let available = adapter.limits();
    let mut limits = wgpu::Limits::default();
    limits.max_storage_buffer_binding_size = available.max_storage_buffer_binding_size.min(1 << 30);
    limits.max_buffer_size = available.max_buffer_size.min(1 << 30);
    limits.max_texture_dimension_2d = available
        .max_texture_dimension_2d
        .max(a.width.max(a.height));
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("sky environment export"),
            required_features: adapter.features() & wgpu::Features::TIMESTAMP_QUERY,
            required_limits: limits,
            ..Default::default()
        })
        .await?;
    eprintln!("adapter: {:?}", adapter.get_info());

    let t0 = Instant::now();
    let mut renderer = Renderer::new(
        &device,
        &model,
        &wavelengths,
        a.ground_albedo,
        config.clone(),
    )?;
    let solve = renderer.rebuild(&device, &queue)?;
    renderer.steps = a.steps.clamp(4, 512);
    let solve_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let resident = renderer.resident_bytes();

    let view = View {
        yaw_deg: 0.0,
        pitch_deg: 0.0,
        fov_y_deg: 90.0,
        altitude_km: a.altitude_km.max(0.0),
        sun_elevation_deg: a.sun_elevation_deg,
        sun_azimuth_deg: a.sun_azimuth_deg,
    };
    renderer.resize(&device, [a.width, a.height]);
    let t1 = Instant::now();
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render_environment(&queue, &mut encoder, view);
    queue.submit([encoder.finish()]);
    device.poll(wgpu::PollType::wait_indefinitely())?;
    let env_ms = t1.elapsed().as_secs_f64() * 1000.0;

    let t2 = Instant::now();
    let radiance_bytes = read_texture(&device, &queue, renderer.target_texture(), 16)?;
    let transmittance_bytes = read_texture(&device, &queue, renderer.transmittance_texture(), 16)?;
    let readback_ms = t2.elapsed().as_secs_f64() * 1000.0;

    // ---- CPU: color conversion, solar disk compositing, serialization ----
    let t3 = Instant::now();
    let width = a.width as usize;
    let height = a.height as usize;
    let texels = width * height;
    let rec2020: &[f32] = bytemuck::cast_slice(&radiance_bytes);
    let tau: &[f32] = bytemuck::cast_slice(&transmittance_bytes);
    if rec2020.len() < texels * 4 || tau.len() < 4 {
        return Err("unexpected readback size".into());
    }

    // Atmosphere-attenuated direct-sun irradiance: sum over the four wave nodes.
    let mut sun_rec2020 = [0.0f32; 3];
    for k in 0..4 {
        let e = wavelengths.solar[k] * tau[k];
        for ch in 0..3 {
            sun_rec2020[ch] += e * wavelengths.rgb[k][ch];
        }
    }
    let mut sun_irradiance = rec2020_to_linear_srgb(sun_rec2020);
    for c in sun_irradiance.iter_mut() {
        *c = (*c * a.exposure_scale).max(0.0);
    }
    if sun_irradiance.iter().any(|c| !c.is_finite()) {
        return Err("non-finite sun irradiance".into());
    }

    // Environment radiance: Rec.2020 -> linear sRGB, negative converted channels clamped.
    let mut radiance = Vec::with_capacity(texels * 3);
    for i in 0..texels {
        let c = [rec2020[i * 4], rec2020[i * 4 + 1], rec2020[i * 4 + 2]];
        let s = rec2020_to_linear_srgb(c);
        for ch in 0..3 {
            let v = s[ch] * a.exposure_scale;
            if !v.is_finite() {
                return Err(format!("non-finite radiance at texel {i}").into());
            }
            radiance.push(v.max(0.0));
        }
    }

    // Composite the separately evaluated solar disk: the environment projection
    // excludes it by contract, so the consumer would otherwise lose the sun.
    let sun_dir = {
        let (el, az) = (
            a.sun_elevation_deg.to_radians(),
            a.sun_azimuth_deg.to_radians(),
        );
        [el.cos() * az.sin(), el.sin(), el.cos() * az.cos()]
    };
    let radius = a.sun_angular_radius_deg.to_radians();
    let cos_radius = radius.cos();
    let solid_angle = 2.0 * std::f32::consts::PI * (1.0 - cos_radius);
    let disc_radiance: [f32; 3] = std::array::from_fn(|ch| sun_irradiance[ch] / solid_angle);
    let theta_sun = sun_dir[1].clamp(-1.0, 1.0).acos();
    let ss = a.sun_super_samples.max(1);
    for y in 0..height {
        let theta = std::f32::consts::PI * (y as f32 + 0.5) / height as f32;
        // Cheap conservative row cull: the disk radius plus one texel of margin.
        if (theta - theta_sun).abs() > radius + 2.0 * std::f32::consts::PI / height as f32 {
            continue;
        }
        for x in 0..width {
            let mut covered = 0.0f32;
            for sy in 0..ss {
                let theta_sub = std::f32::consts::PI * (y as f32 + (sy as f32 + 0.5) / ss as f32)
                    / height as f32;
                let (sin_theta_sub, cos_theta_sub) = theta_sub.sin_cos();
                for sx in 0..ss {
                    let phi = std::f32::consts::TAU * (x as f32 + (sx as f32 + 0.5) / ss as f32)
                        / width as f32;
                    let (sin_phi, cos_phi) = phi.sin_cos();
                    let dir = [
                        sin_phi * sin_theta_sub,
                        cos_theta_sub,
                        cos_phi * sin_theta_sub,
                    ];
                    let cos_angle = dir[0] * sun_dir[0] + dir[1] * sun_dir[1] + dir[2] * sun_dir[2];
                    if cos_angle >= cos_radius {
                        covered += 1.0;
                    }
                }
            }
            let frac = covered / (ss * ss) as f32;
            if frac > 0.0 {
                let base = (y * width + x) * 3;
                for ch in 0..3 {
                    radiance[base + ch] += disc_radiance[ch] * frac;
                }
            }
        }
    }
    let convert_ms = t3.elapsed().as_secs_f64() * 1000.0;

    // ---- Write outputs ----
    let radiance_path = a.out.join("radiance.f32");
    std::fs::write(&radiance_path, bytemuck::cast_slice(&radiance))?;
    let radiance_checksum = sky_realtime::checksum(bytemuck::cast_slice(&radiance));
    write_preview(&a.out.join("preview.png"), width, height, &radiance)?;

    let stats = radiance_stats(width, height, &radiance);
    let adapter_info = adapter.get_info();
    let manifest = json!({
        "kind": "lumina_environment_v1",
        "generator": {
            "package": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
            "solver": "sky-realtime",
            "shader_checksum": sky_realtime::checksum(sky_realtime::shader_source().as_bytes()),
            "adapter": adapter_json(&adapter_info),
        },
        "size": [a.width, a.height],
        "direction_mapping": "equirect_texel_centered_v1",
        "direction_convention": "u=(x+0.5)/W, v=(y+0.5)/H; phi=2*pi*u; theta=pi*v; dir=(sin(phi)*sin(theta), cos(theta), cos(phi)*sin(theta)); row 0 is +Y; longitude 0 is +Z; azimuth 0 deg faces +Z",
        "radiance": {
            "file": "radiance.f32",
            "format": "rgb32f_le_row_major",
            "color_space": "linear_srgb_d65",
            "reference": "scene-referred radiance of the frozen atmosphere at the observer altitude",
            "solar_disk_included": true,
            "checksum": radiance_checksum,
            "stats": stats,
        },
        "sun": {
            "direction": sun_dir,
            "elevation_deg": a.sun_elevation_deg,
            "azimuth_deg": a.sun_azimuth_deg,
            "angular_radius_deg": a.sun_angular_radius_deg,
            "angular_solid_angle_sr": solid_angle,
            "irradiance_rgb": sun_irradiance,
            "disc_radiance_rgb": disc_radiance,
            "compositing": "analytic disk added to the environment after projection; coverage supersampled per texel",
            "transmittance_4wave": [tau[0], tau[1], tau[2], tau[3]],
        },
        "atmosphere": {
            "model": "earth_with_aerosol_scale",
            "aerosol_scale": a.aerosol_scale,
            "ground_albedo": a.ground_albedo,
            "observer_altitude_km": view.altitude_km,
            "config": config,
            "wavelengths": wavelengths,
            "mapping_calibration": serde_json::from_str::<serde_json::Value>(sky_realtime::mapping::calibration_json())?,
            "solve": solve,
            "runtime_steps": renderer.steps,
            "resident_bytes": resident,
        },
        "timing_ms": {
            "solve": solve_ms,
            "environment_projection": env_ms,
            "readback": readback_ms,
            "cpu_convert_and_disk": convert_ms,
        },
        "caveats": [
            "frozen far-field boundary: one observer altitude and one sun pose per asset",
            "excludes atmospheric transport inside a consuming scene; not a joint cloud/atmosphere solve",
            "solar disk is an analytic composition scaled by the exported direct-sun irradiance",
        ],
    });
    std::fs::write(
        a.out.join("environment.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;

    println!(
        "wrote {}x{} environment to {} (radiance checksum {})",
        a.width,
        a.height,
        a.out.display(),
        manifest["radiance"]["checksum"].as_str().unwrap_or("?")
    );
    println!(
        "sun: elevation {:.2} deg, azimuth {:.2} deg, irradiance rgb {:?}",
        a.sun_elevation_deg, a.sun_azimuth_deg, sun_irradiance
    );
    println!(
        "timing ms: solve={:.1} env={:.1} readback={:.1} convert={:.1}",
        solve_ms, env_ms, readback_ms, convert_ms
    );
    Ok(())
}

/// wgpu::AdapterInfo -> JSON（该类型本身不实现 Serialize）
fn adapter_json(info: &wgpu::AdapterInfo) -> serde_json::Value {
    json!({
        "name": info.name,
        "vendor": info.vendor,
        "device": info.device,
        "driver": info.driver,
        "driver_info": info.driver_info,
        "backend": format!("{:?}", info.backend),
        "device_type": format!("{:?}", info.device_type),
    })
}

/// Rec.2020 -> linear sRGB (D65), the same matrix used by the cloud boundary.
fn rec2020_to_linear_srgb(v: [f32; 3]) -> [f32; 3] {
    [
        1.660_491 * v[0] - 0.587_641_1 * v[1] - 0.072_849_9 * v[2],
        -0.124_550_5 * v[0] + 1.132_899_9 * v[1] - 0.008_349_4 * v[2],
        -0.018_150_8 * v[0] - 0.100_578_9 * v[1] + 1.118_729_7 * v[2],
    ]
}

fn radiance_stats(width: usize, height: usize, radiance: &[f32]) -> serde_json::Value {
    let luminance = |i: usize| {
        0.2126 * radiance[i * 3] + 0.7152 * radiance[i * 3 + 1] + 0.0722 * radiance[i * 3 + 2]
    };
    let n = width * height;
    let mut sum = 0.0f64;
    let mut max = 0.0f64;
    let mut samples = Vec::with_capacity(n);
    for i in 0..n {
        let l = luminance(i) as f64;
        sum += l;
        max = max.max(l);
        samples.push(l);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f64| samples[((samples.len() as f64 - 1.0) * p).round() as usize];
    json!({
        "mean_luminance": sum / n as f64,
        "median_luminance": pct(0.5),
        "p95_luminance": pct(0.95),
        "p99_luminance": pct(0.99),
        "max_luminance": max,
    })
}

/// Exposure/Reinhard/sRGB preview; inspection only, never a reference image.
fn write_preview(
    path: &std::path::Path,
    width: usize,
    height: usize,
    radiance: &[f32],
) -> sky_realtime::Result<()> {
    let n = width * height;
    let mut sum = 0.0f64;
    for i in 0..n {
        sum += (0.2126 * radiance[i * 3]
            + 0.7152 * radiance[i * 3 + 1]
            + 0.0722 * radiance[i * 3 + 2]) as f64;
    }
    let mean = (sum / n as f64).max(1e-12) as f32;
    let exposure = 0.18 / mean;
    let mut rgb = Vec::with_capacity(n * 3);
    for i in 0..n * 3 {
        let c = radiance[i] * exposure;
        let mapped = c / (1.0 + c);
        let encoded = if mapped <= 0.003_130_8 {
            12.92 * mapped
        } else {
            1.055 * mapped.powf(1.0 / 2.4) - 0.055
        };
        rgb.push((encoded.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
    }
    image::save_buffer(
        path,
        &rgb,
        width as u32,
        height as u32,
        image::ColorType::Rgb8,
    )?;
    Ok(())
}
