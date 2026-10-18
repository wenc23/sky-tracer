//! CPU/GPU validation harness for the weather and cloud cores.
//!
//! This application only composes public core APIs. It owns no transport or
//! weather equations; outputs are JSON reports plus optional images.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use clap::{Parser, Subcommand};
use serde_json::json;

#[derive(Parser, Debug)]
#[command(version, about = "Weather/cloud validation and benchmark harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Exercise all 20 behaviour states once and record diagnostics.
    WeatherCatalog {
        #[arg(long)]
        out: PathBuf,
        /// Simulated seconds per state (default 120).
        #[arg(long, default_value_t = 120.0)]
        seconds: f64,
    },
    /// Run the automatic director for a climate and record transitions.
    WeatherSimulate {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "temperate")]
        climate: String,
        #[arg(long, default_value_t = 6.0)]
        hours: f64,
        /// Simulated seconds per advance call (default 300).
        #[arg(long, default_value_t = 300.0)]
        chunk_s: f64,
    },
    /// Measure weather stepping and volume sampling on this machine.
    Benchmark {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 200)]
        steps: u32,
        #[arg(long, default_value_t = 10)]
        volumes: u32,
    },
    /// Render the cloud pass headlessly and write PNG + JSON.
    RenderCloud {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 320)]
        width: u32,
        #[arg(long, default_value_t = 180)]
        height: u32,
        #[arg(long, default_value = "rain")]
        kind: String,
        /// Skip weather generation and upload an all-zero volume.
        #[arg(long, default_value_t = false)]
        zero_density: bool,
        /// Trace resolution as a fraction of the output (0.25..=1.0).
        #[arg(long, default_value_t = 1.0)]
        trace_scale: f32,
        /// Sub-grid detail-noise erosion strength (0..=1). 0 keeps the pure
        /// weather-volume look; the workbench enables 0.8/0.9.
        #[arg(long, default_value_t = 0.0)]
        detail_strength: f32,
        /// Sun optical-depth march steps per lit sample (1..=12).
        #[arg(long, default_value_t = 4)]
        sun_steps: u32,
        /// Primary-view ray march steps (8..=256).
        #[arg(long, default_value_t = 96)]
        max_steps: u32,
        /// Stratified samples per trace pixel (1..=16). No temporal history is
        /// used by this audit, so >1 averages step-boundary aliasing.
        #[arg(long, default_value_t = 4)]
        samples: u32,
    },
    /// Verify the GPU weather-effects zero-effect identity against a background.
    EffectsZero {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 64)]
        size: u32,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match Cli::parse().command {
        Command::WeatherCatalog { out, seconds } => weather_catalog(&out, seconds),
        Command::WeatherSimulate {
            out,
            climate,
            hours,
            chunk_s,
        } => weather_simulate(&out, &climate, hours, chunk_s),
        Command::Benchmark {
            out,
            steps,
            volumes,
        } => benchmark(&out, steps, volumes),
        Command::RenderCloud {
            out,
            width,
            height,
            kind,
            zero_density,
            trace_scale,
            detail_strength,
            sun_steps,
            max_steps,
            samples,
        } => pollster::block_on(render_cloud(
            &out,
            width,
            height,
            &kind,
            zero_density,
            trace_scale,
            detail_strength,
            sun_steps,
            max_steps,
            samples,
        )),
        Command::EffectsZero { out, size } => pollster::block_on(effects_zero(&out, size)),
    }
}

fn ensure_new(path: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if path.exists() {
        return Err(format!("output already exists: {}", path.display()).into());
    }
    fs::create_dir_all(path)?;
    Ok(())
}

fn kinds() -> [(&'static str, sky_weather::WeatherKind); 20] {
    use sky_weather::WeatherKind as W;
    [
        ("clear", W::Clear),
        ("fair", W::Fair),
        ("partly_cloudy", W::PartlyCloudy),
        ("overcast", W::Overcast),
        ("fog", W::Fog),
        ("haze", W::Haze),
        ("dust", W::Dust),
        ("drizzle", W::Drizzle),
        ("rain", W::Rain),
        ("heavy_rain", W::HeavyRain),
        ("tropical_shower", W::TropicalShower),
        ("thunderstorm", W::Thunderstorm),
        ("hailstorm", W::Hailstorm),
        ("supercell", W::Supercell),
        ("light_snow", W::LightSnow),
        ("heavy_snow", W::HeavySnow),
        ("blizzard", W::Blizzard),
        ("sleet", W::Sleet),
        ("freezing_rain", W::FreezingRain),
        ("gale", W::Gale),
    ]
}

fn parse_kind(name: &str) -> Result<sky_weather::WeatherKind, String> {
    kinds()
        .into_iter()
        .find(|(label, _)| *label == name)
        .map(|(_, kind)| kind)
        .ok_or_else(|| {
            format!(
                "unknown weather kind {name:?}; use one of {}",
                kinds()
                    .iter()
                    .map(|(label, _)| *label)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn parse_climate(name: &str) -> Result<sky_weather::Climate, String> {
    use sky_weather::Climate as C;
    let climate = match name {
        "tropical" => C::Tropical,
        "temperate" => C::Temperate,
        "continental" => C::Continental,
        "arid" => C::Arid,
        "polar" => C::Polar,
        "alpine" => C::Alpine,
        _ => return Err(format!("unknown climate {name:?}")),
    };
    Ok(climate)
}

fn volume_summary(model: &sky_weather::Model) -> Option<serde_json::Value> {
    let volume = model.sample_volume().ok()?;
    volume.validate().ok()?;
    let mut max_lwc = 0.0f32;
    let mut max_re = 0.0f32;
    for voxel in volume.data.chunks_exact(4) {
        max_lwc = max_lwc.max(voxel[0]);
        max_re = max_re.max(voxel[1]);
    }
    Some(json!({
        "dims": volume.dims,
        "origin_m": volume.origin_m,
        "spacing_m": volume.spacing_m,
        "max_lwc_kg_m3": max_lwc,
        "max_reff_m": max_re,
    }))
}

fn frame_is_finite(frame: &sky_weather::WeatherFrame) -> bool {
    frame.temperature_c.is_finite()
        && frame.cloud_cover.is_finite()
        && frame.lwp_g_m2.is_finite()
        && frame.cloud_base_m.is_finite()
        && frame.cloud_top_m.is_finite()
        && frame.precipitation.rate_mm_h.is_finite()
        && frame.visibility_m.is_finite()
        && frame.wind_speed_m_s.is_finite()
        && frame.lightning_rate_per_min.is_finite()
        && frame.cloud_cover >= 0.0
        && frame.precipitation.rate_mm_h >= 0.0
        && frame.visibility_m >= 0.0
}

fn weather_catalog(
    out: &Path,
    seconds: f64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err("seconds must be positive and finite".into());
    }
    ensure_new(out)?;
    let mut report = Vec::new();
    for (name, kind) in kinds() {
        let mut config = sky_weather::Config::fixed(kind);
        config.grid = [64, 64];
        config.volume_dims = [48, 32, 48];
        config.seed = 0xC10D_2026_1017;
        let mut model = sky_weather::Model::new(config)?;
        let start = Instant::now();
        let mut advance_error = None;
        let mut elapsed = 0.0;
        while elapsed < seconds {
            let chunk = (seconds - elapsed).min(60.0);
            if let Err(error) = model.advance(chunk) {
                advance_error = Some(error.to_string());
                break;
            }
            elapsed += chunk;
        }
        let frame = model.diagnostics();
        let effects = model.effects();
        report.push(json!({
            "kind": name,
            "seconds": seconds,
            "wall_seconds": start.elapsed().as_secs_f64(),
            "advance_error": advance_error,
            "frame": frame,
            "effects": effects,
            "finite": frame_is_finite(&frame),
            "volume": volume_summary(&model),
        }));
        println!("{name}: {:?}", frame);
    }
    fs::write(
        out.join("report.json"),
        serde_json::to_vec_pretty(&json!({
            "kind": "weather_catalog_v2",
            "created_unix_s": unix_seconds(),
            "results": report,
        }))?,
    )?;
    Ok(())
}

fn weather_simulate(
    out: &Path,
    climate_name: &str,
    hours: f64,
    chunk_s: f64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !hours.is_finite() || hours <= 0.0 || !chunk_s.is_finite() || chunk_s <= 0.0 {
        return Err("hours and chunk_s must be positive and finite".into());
    }
    let climate = parse_climate(climate_name)?;
    ensure_new(out)?;
    let mut config = sky_weather::Config::auto(climate);
    config.grid = [64, 64];
    config.volume_dims = [32, 24, 32];
    config.seed = 0xC10D_2026_1017;
    let mut model = sky_weather::Model::new(config)?;
    let start = Instant::now();
    let mut transitions = Vec::new();
    let mut last_kind = model.diagnostics().kind;
    let mut advance_error = None;
    let chunks = ((hours * 3600.0) / chunk_s).ceil() as u32;
    for _ in 0..chunks {
        if let Err(error) = model.advance(chunk_s) {
            advance_error = Some(error.to_string());
            break;
        }
        let frame = model.diagnostics();
        if frame.kind != last_kind {
            transitions.push(json!({
                "time_s": frame.time_s,
                "kind": frame.kind,
                "temperature_c": frame.temperature_c,
                "precipitation_mm_h": frame.precipitation.rate_mm_h,
            }));
            last_kind = frame.kind;
        }
    }
    let final_frame = model.diagnostics();
    fs::write(
        out.join("report.json"),
        serde_json::to_vec_pretty(&json!({
            "kind": "weather_simulate_v2",
            "created_unix_s": unix_seconds(),
            "climate": climate_name,
            "hours": hours,
            "chunk_s": chunk_s,
            "wall_seconds": start.elapsed().as_secs_f64(),
            "advance_error": advance_error,
            "transitions": transitions,
            "final": final_frame,
            "finite": frame_is_finite(&final_frame),
            "volume": volume_summary(&model),
        }))?,
    )?;
    println!("{final_frame:?}");
    Ok(())
}

fn benchmark(
    out: &Path,
    steps: u32,
    volumes: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if steps == 0 || volumes == 0 {
        return Err("steps and volumes must be positive".into());
    }
    ensure_new(out)?;
    let mut config = sky_weather::Config::fixed(sky_weather::WeatherKind::Rain);
    config.seed = 0xC10D_2026_1017;
    let mut model = sky_weather::Model::new(config)?;
    model.advance(300.0)?;
    let _ = model.sample_volume()?;
    let start = Instant::now();
    for _ in 0..steps {
        model.step()?;
    }
    let step_s = start.elapsed().as_secs_f64() / f64::from(steps);
    let start = Instant::now();
    let mut voxels = 0u64;
    for _ in 0..volumes {
        let volume = model.sample_volume()?;
        voxels = volume.data.len() as u64 / 4;
    }
    let volume_s = start.elapsed().as_secs_f64() / f64::from(volumes);
    let report = json!({
        "kind": "weather_benchmark_v1",
        "created_unix_s": unix_seconds(),
        "steps": steps,
        "volumes": volumes,
        "step_ms": step_s * 1000.0,
        "volume_ms": volume_s * 1000.0,
        "volume_voxels": voxels,
        "note": "CPU wall-clock only; no GPU, no rendering, no I/O.",
    });
    fs::write(
        out.join("benchmark.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{report}");
    Ok(())
}

async fn create_gpu()
-> Result<(wgpu::Device, wgpu::Queue), Box<dyn std::error::Error + Send + Sync>> {
    let adapter = wgpu::Instance::default()
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        })
        .await
        .map_err(|e| format!("no wgpu adapter: {e}"))?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("cloud audit device"),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            ..Default::default()
        })
        .await?;
    Ok((device, queue))
}

async fn render_cloud(
    out: &Path,
    width: u32,
    height: u32,
    kind_name: &str,
    zero_density: bool,
    trace_scale: f32,
    detail_strength: f32,
    sun_steps: u32,
    max_steps: u32,
    samples: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err("image dimensions must be in 1..=4096".into());
    }
    ensure_new(out)?;
    let kind = parse_kind(kind_name)?;
    let (volume, diagnostics) = if zero_density {
        let dims = [48, 24, 48];
        (
            sky_weather::CloudVolume {
                dims,
                origin_m: [-16_000.0, 0.0, -16_000.0],
                spacing_m: [333.0, 291.0, 333.0],
                data: vec![0.0; (dims[0] * dims[1] * dims[2] * 4) as usize],
            },
            None,
        )
    } else {
        let mut config = sky_weather::Config::fixed(kind);
        config.grid = [64, 64];
        config.dx_m = 1_000.0;
        config.volume_dims = [96, 48, 96];
        config.volume_extent_m = [32_000.0, 14_000.0, 32_000.0];
        let mut model = sky_weather::Model::new(config)?;
        for _ in 0..2 {
            model.advance(600.0)?;
        }
        let diagnostics = model.diagnostics();
        (model.sample_volume()?, Some(diagnostics))
    };
    let (device, queue) = create_gpu().await?;
    let cloud_config = sky_clouds::Config {
        volume_dims: volume.dims,
        max_steps,
        samples,
        sun_steps,
        ambient_scale: 0.35,
        trace_scale,
        detail_strength,
        ..sky_clouds::Config::default()
    };
    let mut renderer = sky_clouds::Renderer::new(&device, &queue, cloud_config)?;
    renderer.upload_volume(
        &queue,
        &volume.data,
        sky_clouds::VolumeDescriptor {
            dims: volume.dims,
            origin_m: volume.origin_m,
            spacing_m: volume.spacing_m,
        },
    )?;
    renderer.resize(&device, [width, height]);

    let (env_tex, env_view) = rgba32_texture(&device, [64, 32]);
    write_rgba32(&queue, &env_tex, [64, 32], &environment_gradient(64, 32));
    let (sun_tex, sun_view) = rgba32_texture(&device, [1, 1]);
    write_rgba32(&queue, &sun_tex, [1, 1], &[1.0, 1.0, 1.0, 1.0]);
    let (bg_tex, bg_view) = rgba32_texture(&device, [width, height]);
    let background = [0.16, 0.28, 0.55, 1.0];
    let background_pixels = background.repeat((width * height) as usize);
    write_rgba32(&queue, &bg_tex, [width, height], &background_pixels);
    renderer.set_environment(
        &device,
        &env_view,
        &sun_view,
        [
            [1.0, 0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
        ],
    );
    renderer.set_background(&device, &bg_view);

    let elevation = 35.0f32.to_radians();
    let azimuth = 180.0f32.to_radians();
    let sun = sky_clouds::Sun {
        direction: [
            azimuth.sin() * elevation.cos(),
            elevation.sin(),
            azimuth.cos() * elevation.cos(),
        ],
    };
    let camera = sky_clouds::Camera {
        origin_m: [0.0, 200.0, 0.0],
        yaw_deg: 0.0,
        pitch_deg: 18.0,
        fov_y_deg: 65.0,
    };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("cloud audit encoder"),
    });
    renderer.render(&queue, &mut encoder, camera, sun)?;
    let row_bytes = width as usize * 16;
    let padded_row = row_bytes.div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cloud audit readback"),
        size: (padded_row * height as usize) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: renderer.target_texture(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row as u32),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely())?;
    let mapped = slice.get_mapped_range().to_vec();
    let mut pixels = vec![0.0f32; (width * height * 4) as usize];
    for y in 0..height as usize {
        let src = y * padded_row;
        let dst = y * row_bytes;
        let row: &[f32] = bytemuck::cast_slice(&mapped[src..src + row_bytes]);
        pixels[dst / 4..dst / 4 + width as usize * 4].copy_from_slice(row);
    }
    let mut max_zero_error = 0.0f32;
    let mut mean = 0.0f64;
    let mut nonzero = 0u64;
    for chunk in pixels.chunks_exact(4) {
        for c in 0..3 {
            mean += f64::from(chunk[c]);
            if chunk[c] > 1.0e-5 {
                nonzero += 1;
            }
        }
        if zero_density {
            for c in 0..3 {
                max_zero_error = max_zero_error.max((chunk[c] - background[c]).abs());
            }
        }
    }
    mean /= (width * height * 3) as f64;
    let preview = tonemap_rgba(&pixels);
    image::save_buffer(
        out.join("preview.png"),
        &preview,
        width,
        height,
        image::ColorType::Rgba8,
    )?;
    fs::write(out.join("radiance.f32"), bytemuck::cast_slice(&pixels))?;
    let report = json!({
        "kind": "cloud_render_v1",
        "created_unix_s": unix_seconds(),
        "preset": if zero_density { "zero_density" } else { kind_name },
        "width": width,
        "height": height,
        "half_res": trace_scale < 0.999,
        "trace_scale": trace_scale,
        "trace": renderer.stats().last_trace,
        "volume_dims": volume.dims,
        "detail_strength": detail_strength,
        "sun_steps": sun_steps,
        "max_steps": max_steps,
        "samples": samples,
        "diagnostics": diagnostics,
        "mean_radiance": mean,
        "nonzero_channel_fraction": nonzero as f64 / (width * height * 3) as f64,
        "zero_density_max_abs_error": if zero_density { Some(max_zero_error) } else { None },
        "background": background,
        "notes": [
            "linear Rec.2020 f32 radiance; preview.png is tonemapped for viewing",
            "v1 gray visible optics, two-lobe HG, fixed MS octaves",
            "aerial perspective and ground cloud shadows are optional Config features, disabled in this audit; single frame, no temporal history"
        ],
    });
    fs::write(out.join("report.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("{report}");
    Ok(())
}

async fn effects_zero(
    out: &Path,
    size: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !(1..=1024).contains(&size) {
        return Err("effects-zero size must be in 1..=1024".into());
    }
    ensure_new(out)?;
    let (device, queue) = create_gpu().await?;

    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let fx = x as f32 / (size.max(2) - 1) as f32;
            let fy = y as f32 / (size.max(2) - 1) as f32;
            pixels.extend_from_slice(&[0.05 + 0.3 * fx, 0.1 + 0.4 * fy, 0.8 - 0.3 * fx, 1.0]);
        }
    }
    let (background, background_view) = rgba32_texture(&device, [size, size]);
    write_rgba32(&queue, &background, [size, size], &pixels);

    let mut effects =
        sky_weather_fx::Renderer::new(&device, &queue, sky_weather_fx::Config::default())?;
    effects.resize(&device, [size, size]);
    effects.set_background(&device, &background_view);
    let input = sky_weather_fx::Input {
        time_s: 0.0,
        camera: sky_weather_fx::Camera {
            origin_m: [0.0, 200.0, 0.0],
            yaw_deg: 0.0,
            pitch_deg: 15.0,
            fov_y_deg: 60.0,
        },
        sun: sky_weather_fx::Sun {
            direction: [0.4, 0.6, 0.2],
        },
        rain: 0.0,
        snow: 0.0,
        hail: 0.0,
        sleet: 0.0,
        fog: 0.0,
        dust: 0.0,
        wind: [0.0, 0.0],
        cloud_darkening: 0.0,
        rainbow: 0.0,
        lightning_flash: 0.0,
        lightning_seed: 0,
    };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("effects zero audit encoder"),
    });
    effects.render(&queue, &mut encoder, input)?;
    queue.submit([encoder.finish()]);
    let output = read_rgba32(&device, &queue, effects.target_texture(), size, size)?;
    let mut max_error = 0.0f32;
    let mut mean_error = 0.0f64;
    for (actual, expected) in output.chunks_exact(4).zip(pixels.chunks_exact(4)) {
        for channel in 0..4 {
            let error = (actual[channel] - expected[channel]).abs();
            max_error = max_error.max(error);
            mean_error += f64::from(error);
        }
    }
    mean_error /= (size * size * 4) as f64;
    let report = json!({
        "kind": "effects_zero_v1",
        "created_unix_s": unix_seconds(),
        "size": size,
        "max_abs_error": max_error,
        "mean_abs_error": mean_error,
        "pass": max_error <= 1.0e-5,
        "note": "Rgba32Float background copied through the all-zero weather effects shader",
    });
    fs::write(out.join("report.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("{report}");
    Ok(())
}

fn read_rgba32(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Result<Vec<f32>, Box<dyn std::error::Error + Send + Sync>> {
    let row_bytes = width as usize * 16;
    let padded_row = row_bytes.div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rgba32 readback"),
        size: (padded_row * height as usize) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("rgba32 readback encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row as u32),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely())?;
    let mapped = slice.get_mapped_range().to_vec();
    let mut pixels = vec![0.0f32; (width * height * 4) as usize];
    for y in 0..height as usize {
        let src = y * padded_row;
        let dst = y * row_bytes;
        let row: &[f32] = bytemuck::cast_slice(&mapped[src..src + row_bytes]);
        pixels[dst / 4..dst / 4 + width as usize * 4].copy_from_slice(row);
    }
    Ok(pixels)
}

fn rgba32_texture(device: &wgpu::Device, size: [u32; 2]) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cloud audit texture"),
        size: wgpu::Extent3d {
            width: size[0].max(1),
            height: size[1].max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    (texture, view)
}

fn write_rgba32(queue: &wgpu::Queue, texture: &wgpu::Texture, size: [u32; 2], pixels: &[f32]) {
    let row_bytes = size[0] as usize * 16;
    let row_floats = size[0] as usize * 4;
    let padded = row_bytes.div_ceil(256) * 256;
    let mut bytes = vec![0u8; padded * size[1] as usize];
    for y in 0..size[1] as usize {
        let src = y * row_floats;
        let dst = y * padded;
        bytes[dst..dst + row_bytes]
            .copy_from_slice(bytemuck::cast_slice(&pixels[src..src + row_floats]));
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(padded as u32),
            rows_per_image: Some(size[1]),
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
}

fn environment_gradient(width: u32, height: u32) -> Vec<f32> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        let t = y as f32 / (height.max(2) - 1) as f32;
        for _ in 0..width {
            out.extend_from_slice(&[
                0.2 + 0.2 * (1.0 - t),
                0.35 + 0.3 * (1.0 - t),
                0.7 + 0.2 * (1.0 - t),
                1.0,
            ]);
        }
    }
    out
}

fn tonemap_rgba(pixels: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len());
    for chunk in pixels.chunks_exact(4) {
        for c in 0..3 {
            let v = chunk[c].max(0.0);
            let mapped = v / (1.0 + v);
            let srgb = if mapped <= 0.003_130_8 {
                12.92 * mapped
            } else {
                1.055 * mapped.powf(1.0 / 2.4) - 0.055
            };
            out.push((srgb.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
        out.push(255);
    }
    out
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
