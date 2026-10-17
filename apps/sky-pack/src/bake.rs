use std::path::Path;
use std::time::Instant;

use serde_json::json;
use sky_realtime::mapping;
use sky_realtime::model::Model;
use sky_realtime::{Config, Renderer, Wavelengths};

use crate::BakeArgs;
use crate::pack::{AtlasLayout, SKY_LUT_SIZE, build_atlas, generated_constants};

pub async fn run(args: BakeArgs) -> Result<(), String> {
    if !(0.0..=16.0).contains(&args.aerosol_scale) || !args.aerosol_scale.is_finite() {
        return Err("aerosol scale must be finite and in [0, 16]".into());
    }
    if !(0.0..=1.0).contains(&args.ground_albedo) {
        return Err("ground albedo must be in [0, 1]".into());
    }
    if !(4..=512).contains(&args.steps) {
        return Err("steps must be in [4, 512]".into());
    }
    if !args.template.is_dir() {
        return Err(format!(
            "shader template {} does not exist (run from the repository root)",
            args.template.display()
        ));
    }

    // ---- GPU: solve the fixed medium exactly once, then read the fields back.
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        })
        .await
        .map_err(|e| format!("request adapter: {e}"))?;
    let adapter_info = adapter.get_info();
    let available = adapter.limits();
    let mut limits = wgpu::Limits::default();
    limits.max_texture_dimension_2d = available.max_texture_dimension_2d.min(16384);
    limits.max_storage_buffer_binding_size = available.max_storage_buffer_binding_size.min(1 << 30);
    limits.max_buffer_size = available.max_buffer_size.min(1 << 30);
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("sky-pack bake"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            ..Default::default()
        })
        .await
        .map_err(|e| format!("request device: {e}"))?;

    let model = Model::earth_with_aerosol_scale(args.aerosol_scale).map_err(|e| e.to_string())?;
    let wavelengths = Wavelengths::optimized_four();
    let config = Config::balanced();
    let mut renderer = Renderer::new(
        &device,
        &model,
        &wavelengths,
        args.ground_albedo,
        config.clone(),
    )
    .map_err(|e| e.to_string())?;
    renderer.steps = args.steps;
    let solve_start = Instant::now();
    let solve = renderer
        .rebuild(&device, &queue)
        .map_err(|e| e.to_string())?;
    let solve_ms = solve_start.elapsed().as_secs_f64() * 1000.0;
    let read_start = Instant::now();
    let export = renderer
        .export_pack(&device, &queue)
        .map_err(|e| e.to_string())?;
    let readback_ms = read_start.elapsed().as_secs_f64() * 1000.0;
    let (atlas, layout) = build_atlas(&export);
    let data_glsl = generated_constants(
        &export.tables,
        &layout,
        [export.source.width, export.source.height],
    );

    // ---- Write the pack. Never delete a directory that is not ours.
    let out = &args.out;
    if out.exists() {
        let marker = out.join("manifest.json");
        if !marker.exists() {
            return Err(format!(
                "{} already exists and is not a Sky Tracer bake; choose another --out",
                out.display()
            ));
        }
        std::fs::remove_dir_all(out).map_err(|e| format!("clean {}: {e}", out.display()))?;
    }
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    copy_tree(&args.template, out)?;

    let shaders = out.join("shaders");
    let textures = shaders.join("textures");
    std::fs::create_dir_all(&textures).map_err(|e| e.to_string())?;

    write_bytes(&textures.join("tau.bin"), &export.optical_depth.bytes)?;
    write_bytes(&textures.join("source.bin"), &export.source.bytes)?;
    write_bytes(&textures.join("atlas.bin"), bytemuck::cast_slice(&atlas))?;
    write_text(&shaders.join("lib/atmosphere_data.glsl"), &data_glsl)?;

    // Materialise shaders.properties from the tracked template.
    let template_path = out.join("shaders/shaders.properties.template");
    let template = std::fs::read_to_string(&template_path)
        .map_err(|e| format!("read {}: {e}", template_path.display()))?;
    let properties = render_properties(
        &template,
        &layout,
        export.optical_depth.width,
        export.optical_depth.height,
        export.source.width,
        export.source.height,
    );
    write_text(&shaders.join("shaders.properties"), &properties)?;
    std::fs::remove_file(&template_path).map_err(|e| e.to_string())?;

    // GPL-3.0 requires the licence to travel with the binaries/sources.
    if let Ok(license) = std::fs::read("LICENSE") {
        write_bytes(&out.join("LICENSE"), &license)?;
    }
    write_pack_icon(&out.join("pack.png"))?;

    let atlas_bytes = (atlas.len() * 4) as u64;
    let manifest = json!({
        "kind": "sky_tracer_shader_pack_v1",
        "generator": {
            "package": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
            "solver": "sky-realtime",
            "solver_shader_checksum": sky_realtime::checksum(sky_realtime::shader_source().as_bytes()),
            "adapter": {
                "name": adapter_info.name,
                "backend": format!("{:?}", adapter_info.backend),
                "driver": adapter_info.driver,
            },
        },
        "pack": {
            "name": "SkyTracer",
            "target": "Iris 1.7+ (MC 1.20.1/1.21.x recommended); OptiFine untested",
            "runtime_steps": args.steps,
            "sky_view": {"size": SKY_LUT_SIZE, "format": "RGBA16F", "encoding": "log linear Rec.2020"},
        },
        "atmosphere": {
            "model": "earth_with_aerosol_scale",
            "aerosol_scale": args.aerosol_scale,
            "ground_albedo": args.ground_albedo,
            "observer_note": "pack rebuilds the view LUT per frame; only the fixed medium is baked",
            "config": config,
            "wavelengths": wavelengths,
            "solve": solve,
            "mapping_calibration": serde_json::from_str::<serde_json::Value>(mapping::calibration_json())
                .map_err(|e| e.to_string())?,
        },
        "tables": {
            "optical_depth": {
                "file": "shaders/textures/tau.bin",
                "format": "RGBA16F LE",
                "size": [export.optical_depth.width, export.optical_depth.height],
                "bytes": export.optical_depth.bytes.len(),
                "checksum": sky_realtime::checksum(&export.optical_depth.bytes),
            },
            "source": {
                "file": "shaders/textures/source.bin",
                "format": "RGBA16F LE",
                "size": [export.source.width, export.source.height],
                "bytes": export.source.bytes.len(),
                "checksum": sky_realtime::checksum(&export.source.bytes),
            },
            "atlas": {
                "file": "shaders/textures/atlas.bin",
                "format": "RGBA32F LE",
                "size": [layout.width, layout.rows],
                "bytes": atlas_bytes,
                "layout": {
                    "profile_row": layout.row_profile,
                    "profile_rows": crate::pack::PROFILE_ROWS,
                    "profile_nodes": export.tables.profile_nodes,
                    "heights_row": layout.row_heights,
                    "height_nodes": export.tables.dims[0],
                    "ground_row": layout.row_ground,
                    "mean_row": layout.row_mean,
                    "phase_row": layout.row_phase,
                    "solar_row": layout.row_solar,
                    "high_row": layout.row_high,
                    "high_rows": layout.high_rows,
                },
                "checksum": sky_realtime::checksum(bytemuck::cast_slice(&atlas)),
            },
            "total_bytes": export.optical_depth.bytes.len() as u64
                + export.source.bytes.len() as u64
                + atlas_bytes,
        },
        "timing_ms": {"solve": solve_ms, "readback": readback_ms},
        "caveats": [
            "the pack replaces the sky; terrain and water use a minimal gbuffer program that keeps the vanilla texture, lightmap and fog",
            "no clouds, moon or stars: the atmosphere model is clear-sky and has no night emission",
            "sky is painted where the depth buffer has no geometry; weather that does not write depth needs rain.depth=true",
            "the SkyView buffer must be RGBA16F: its format directive has to be on its own line inside a block comment, otherwise Iris leaves the buffer at RGBA8 and the log-encoded radiance clamps",
            "Sodium skips chunk geometry unless the pack provides a gbuffers_terrain program",
        ],
    });
    write_text(
        &out.join("manifest.json"),
        &serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?,
    )?;
    // A zip next to the folder is what most launchers expect; store entries
    // uncompressed so the tool has no extra dependency and the textures stay
    // bit-identical to the folder.
    let pack_name = out
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "SkyTracer".to_owned());
    let zip_path = out
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{pack_name}.zip"));
    write_zip(&out.to_owned(), &zip_path, &pack_name)?;

    println!(
        "baked SkyTracer to {} ({:.1} MB of tables)",
        out.display(),
        manifest["tables"]["total_bytes"].as_u64().unwrap_or(0) as f64 / (1024.0 * 1024.0)
    );
    println!(
        "adapter: {} ({:?}); solve {:.1} ms; readback {:.1} ms",
        adapter_info.name, adapter_info.backend, solve_ms, readback_ms
    );
    println!(
        "tables: tau {}x{}, source {}x{}, atlas {}x{}",
        export.optical_depth.width,
        export.optical_depth.height,
        export.source.width,
        export.source.height,
        layout.width,
        layout.rows
    );
    println!("zip: {}", zip_path.display());
    Ok(())
}

fn render_properties(
    template: &str,
    layout: &AtlasLayout,
    tau_width: u32,
    tau_height: u32,
    source_width: u32,
    source_height: u32,
) -> String {
    template
        .replace("@LUT_SIZE@", &SKY_LUT_SIZE.to_string())
        .replace("@TAU_WIDTH@", &tau_width.to_string())
        .replace("@TAU_HEIGHT@", &tau_height.to_string())
        .replace("@ATLAS_WIDTH@", &layout.width.to_string())
        .replace("@ATLAS_ROWS@", &layout.rows.to_string())
        .replace("@SOURCE_WIDTH@", &source_width.to_string())
        .replace("@SOURCE_HEIGHT@", &source_height.to_string())
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    write_bytes(path, text.as_bytes())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(source).map_err(|e| format!("read {}: {e}", source.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if from.is_dir() {
            std::fs::create_dir_all(&to).map_err(|e| e.to_string())?;
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .map_err(|e| format!("copy {} -> {}: {e}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// Small shader-pack icon: physical sky gradient with a solar disk and the
/// horizon line. Presentation only; not part of the transport.
fn write_pack_icon(path: &Path) -> Result<(), String> {
    let size = 128u32;
    let mut image = image::RgbImage::new(size, size);
    for y in 0..size {
        for x in 0..size {
            let v = y as f32 / (size - 1) as f32;
            let u = x as f32 / (size - 1) as f32;
            let horizon = 0.62f32;
            let color = if v < horizon {
                let t = (v / horizon).powf(0.75);
                let zenith = [26.0f32, 52.0, 112.0];
                let low = [150.0f32, 184.0, 226.0];
                [
                    zenith[0] + (low[0] - zenith[0]) * t,
                    zenith[1] + (low[1] - zenith[1]) * t,
                    zenith[2] + (low[2] - zenith[2]) * t,
                ]
            } else {
                let t = ((v - horizon) / (1.0 - horizon)).powf(0.8);
                let near = [96.0f32, 96.0, 100.0];
                let far = [58.0f32, 58.0, 62.0];
                [
                    near[0] + (far[0] - near[0]) * t,
                    near[1] + (far[1] - near[1]) * t,
                    near[2] + (far[2] - near[2]) * t,
                ]
            };
            let dx = u - 0.70;
            let dy = v - 0.34;
            let distance = (dx * dx + dy * dy).sqrt();
            let sun = (1.0 - (distance / 0.055).min(1.0)).powf(2.0);
            let glow = (1.0 - (distance / 0.25).min(1.0)).powf(3.0);
            let color = [
                color[0] + sun * 220.0 + glow * 30.0,
                color[1] + sun * 210.0 + glow * 26.0,
                color[2] + sun * 180.0 + glow * 18.0,
            ];
            image.put_pixel(
                x,
                y,
                image::Rgb([
                    color[0].clamp(0.0, 255.0) as u8,
                    color[1].clamp(0.0, 255.0) as u8,
                    color[2].clamp(0.0, 255.0) as u8,
                ]),
            );
        }
    }
    image
        .save(path)
        .map_err(|e| format!("save {}: {e}", path.display()))
}

/// Store-only ZIP writer (no compression dependency). Shader packs are plain
/// resource folders, so a stored archive is smaller in code than in bytes.
fn write_zip(pack_dir: &Path, zip_path: &Path, root: &str) -> Result<(), String> {
    let mut files = Vec::new();
    collect_zip_files(pack_dir, pack_dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut data = Vec::new();
    let mut central = Vec::new();
    for (relative, bytes) in &files {
        let name = format!("{root}/{relative}");
        let name_bytes = name.as_bytes();
        let crc = crc32(bytes);
        let offset = data.len() as u32;
        push_u32(&mut data, 0x0403_4b50);
        push_u16(&mut data, 20);
        push_u16(&mut data, 0);
        push_u16(&mut data, 0); // store
        push_u16(&mut data, 0); // time
        push_u16(&mut data, 0); // date
        push_u32(&mut data, crc);
        push_u32(&mut data, bytes.len() as u32);
        push_u32(&mut data, bytes.len() as u32);
        push_u16(&mut data, name_bytes.len() as u16);
        push_u16(&mut data, 0);
        data.extend_from_slice(name_bytes);
        data.extend_from_slice(bytes);

        push_u32(&mut central, 0x0201_4b50);
        push_u16(&mut central, 20);
        push_u16(&mut central, 20);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, crc);
        push_u32(&mut central, bytes.len() as u32);
        push_u32(&mut central, bytes.len() as u32);
        push_u16(&mut central, name_bytes.len() as u16);
        push_u16(&mut central, 0); // extra
        push_u16(&mut central, 0); // comment
        push_u16(&mut central, 0); // disk
        push_u16(&mut central, 0); // internal attrs
        push_u32(&mut central, 0); // external attrs
        push_u32(&mut central, offset);
        central.extend_from_slice(name_bytes);
    }
    let central_offset = data.len() as u32;
    let central_size = central.len() as u32;
    data.extend_from_slice(&central);
    push_u32(&mut data, 0x0605_4b50);
    push_u16(&mut data, 0);
    push_u16(&mut data, 0);
    push_u16(&mut data, files.len() as u16);
    push_u16(&mut data, files.len() as u16);
    push_u32(&mut data, central_size);
    push_u32(&mut data, central_offset);
    push_u16(&mut data, 0);
    write_bytes(zip_path, &data)
}

fn collect_zip_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(directory)
        .map_err(|e| format!("read {}: {e}", directory.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_zip_files(root, &path, files)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            let bytes =
                std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
            files.push((relative, bytes));
        }
    }
    Ok(())
}

fn push_u16(buffer: &mut Vec<u8>, value: u16) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(buffer: &mut Vec<u8>, value: u32) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_template_replaces_every_token() {
        let template = "size=@LUT_SIZE@ tau=@TAU_WIDTH@x@TAU_HEIGHT@ \
                        atlas=@ATLAS_WIDTH@x@ATLAS_ROWS@ src=@SOURCE_WIDTH@x@SOURCE_HEIGHT@";
        let layout = AtlasLayout {
            width: 1024,
            rows: 114,
            row_profile: 0,
            row_heights: 7,
            row_ground: 8,
            row_mean: 9,
            row_phase: 49,
            row_solar: 53,
            row_high: 93,
            high_rows: 21,
        };
        let rendered = render_properties(template, &layout, 512, 128, 3200, 240);
        assert!(!rendered.contains('@'), "{rendered}");
        assert!(rendered.contains("atlas=1024x114"));
    }
}
