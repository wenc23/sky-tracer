//! Offline validation: compile the shipped pack GLSL with a real OpenGL driver,
//! render the same views as `sky-realtime`'s per-pixel transport, and compare.
//!
//! This is the substitute for an in-game test in this repository: Minecraft is
//! not installed, but OpenGL 4.6 is available, so the actual `#version 330
//! compatibility` pack sources run unmodified. The comparison target is the
//! crate's own `render` pipeline with `use_sky_view = false`, i.e. the direct
//! 64-step four-wave integration the pack's LUT is an approximation of.

use std::path::Path;

use glam::Mat4;
use serde::Serialize;
use sky_realtime::model::Model;
use sky_realtime::{Config, Renderer, View, Wavelengths};

use glow::HasContext;

use crate::PreviewArgs;
use crate::gl::{
    FullscreenQuad, GlContext, GlFramebuffer, GlProgram, GlTexture, bind_sampler,
    compile_fragment_source, compile_vertex_source, texture_r32f, texture_rgba8, texture_rgba16f,
    texture_rgba32f,
};
use crate::glsl::{PackData, expand_text};

#[derive(Serialize, Clone, Debug)]
pub struct Metrics {
    pub mean_relative_difference: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
    pub pixels: usize,
}

#[derive(Serialize)]
pub struct ScenarioReport {
    pub sun_elevation_deg: f32,
    pub sun_azimuth_deg: f32,
    pub linear_radiance_difference: Metrics,
    /// Ratio of (pack with disk - pack without disk) to (reference with disk -
    /// reference without disk), averaged over pixels inside the solar disk.
    pub solar_disk_ratio: Option<f64>,
    pub sky_view_build_ms: f64,
    pub composite_ms: f64,
    pub images: Vec<String>,
}

#[derive(Serialize)]
pub struct PassThroughReport {
    pub depth_masked_pixels_unchanged: bool,
    pub other_dimension_pixels_unchanged: bool,
    pub max_byte_difference: i32,
}

#[derive(Serialize)]
pub struct HarnessReport {
    pub kind: &'static str,
    pub gl_renderer: String,
    pub pack: String,
    pub width: u32,
    pub height: u32,
    pub fov_y_deg: f32,
    pub observer_altitude_km: f32,
    pub runtime_steps: u32,
    pub reference: &'static str,
    pub scenarios: Vec<ScenarioReport>,
    pub pass_through: PassThroughReport,
    pub generated_by: &'static str,
}

const PACK_LINEAR_DEBUG: &[&str] = &["#define SKY_PACK_LINEAR_DEBUG 1"];
const SUN_DISK_DEFINE: &str = "#define SKY_SUN_DISK";

const DISPLAY_SHADER: &str = r#"#version 330 compatibility

uniform sampler2D source;
uniform float exposure;
in vec2 texcoord;
layout(location = 0) out vec4 color;

#include "/lib/display.glsl"

void main() {
    vec3 radiance = texture(source, texcoord).rgb;
    color = vec4(display_from_rec2020(radiance, 0.10 * exposure, 1.1), 1.0);
}
"#;

pub async fn run(args: &PreviewArgs) -> Result<HarnessReport, String> {
    let pack = PackData::load(&args.pack)?;
    std::fs::create_dir_all(&args.out).map_err(|e| e.to_string())?;
    let width = args.width.max(16);
    let height = args.height.max(16);
    let elevations = parse_elevations(&args.sun_elevations)?;

    // ---- Reference renderer (same crate, direct per-pixel transport).
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        })
        .await
        .map_err(|e| format!("request adapter: {e}"))?;
    let mut limits = wgpu::Limits::default();
    limits.max_texture_dimension_2d = adapter.limits().max_texture_dimension_2d.min(16384);
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("sky-pack preview reference"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            ..Default::default()
        })
        .await
        .map_err(|e| format!("request device: {e}"))?;
    let model = Model::earth_with_aerosol_scale(pack.aerosol_scale).map_err(|e| e.to_string())?;
    let wavelengths = Wavelengths::optimized_four();
    let mut reference = Renderer::new(
        &device,
        &model,
        &wavelengths,
        pack.ground_albedo,
        Config::balanced(),
    )
    .map_err(|e| e.to_string())?;
    reference.steps = pack.runtime_steps;
    reference.use_sky_view = false;
    reference.segment_km = 0.0;
    reference.spectral_output = false;
    reference
        .rebuild(&device, &queue)
        .map_err(|e| e.to_string())?;
    reference.resize(&device, [width, height]);

    // ---- OpenGL side: the real pack shaders.
    let context = GlContext::new()?;
    let gl = &context.gl;
    unsafe {
        gl.disable(glow::DEPTH_TEST);
        gl.disable(glow::BLEND);
        gl.disable(glow::CULL_FACE);
    }
    let quad = FullscreenQuad::new(gl);

    let tau_bytes = pack.tau.bytes()?;
    let atlas_bytes = pack.atlas.bytes()?;
    let source_bytes = pack.source.bytes()?;
    let atlas_f32: Vec<f32> = bytemuck::cast_slice(&atlas_bytes).to_vec();
    let tau = texture_rgba16f(gl, pack.tau.width, pack.tau.height, Some(&tau_bytes));
    let atlas = texture_rgba32f(gl, pack.atlas.width, pack.atlas.height, Some(&atlas_f32));
    let source = texture_rgba16f(
        gl,
        pack.source.width,
        pack.source.height,
        Some(&source_bytes),
    );

    let scene = make_scene_texture(gl, width, height);
    let depth_one = texture_r32f(gl, width, height, &vec![1.0f32; (width * height) as usize]);
    let mut depth_test = vec![1.0f32; (width * height) as usize];
    for row in 0..height / 2 {
        for column in 0..width {
            depth_test[(row * width + column) as usize] = 0.9;
        }
    }
    let depth_partial = texture_r32f(gl, width, height, &depth_test);
    let black_scene = texture_rgba8(gl, 1, 1, Some(&[0, 0, 0, 255]));

    let lut_fbo = GlFramebuffer::new(gl, pack.lut_size, pack.lut_size, glow::RGBA16F, 8)?;
    let linear_fbo = GlFramebuffer::new(gl, width, height, glow::RGBA32F, 16)?;
    let display_fbo = GlFramebuffer::new(gl, width, height, glow::RGBA8, 4)?;

    // Compile every program once. Errors here are shader-pack bugs.
    // Iris/OptiFine recognise RGBA16F as a buffer-format token; plain GLSL does
    // not, so give it the GL enum value while keeping the shipped line intact.
    let deferred_source =
        pack.prepare_fragment("deferred.fsh", &["#define RGBA16F 0x881A"], &[])?;
    let deferred = GlProgram::new(gl, &deferred_source, "deferred.fsh")?;
    let linear_no_disk_source =
        pack.prepare_fragment("composite.fsh", PACK_LINEAR_DEBUG, &[SUN_DISK_DEFINE])?;
    let composite_linear_no_disk = match GlProgram::new(
        gl,
        &linear_no_disk_source,
        "composite.fsh (linear, no disk)",
    ) {
        Ok(program) => program,
        Err(error) => {
            let dump = args.out.join("failed_composite_linear_no_disk.fsh");
            let _ = std::fs::write(&dump, &linear_no_disk_source);
            return Err(format!("{error}\nsource dumped to {}", dump.display()));
        }
    };
    let composite_linear_disk = GlProgram::new(
        gl,
        &pack.prepare_fragment("composite.fsh", PACK_LINEAR_DEBUG, &[])?,
        "composite.fsh (linear, disk)",
    )?;
    let composite_display = GlProgram::new(
        gl,
        &pack.prepare_fragment("composite.fsh", &[], &[])?,
        "composite.fsh (display)",
    )?;
    let display_shader = expand_text(DISPLAY_SHADER, &pack.shaders)?;
    let display = GlProgram::new(gl, &display_shader, "display.glsl")?;

    // The tracked pack vertex/fragment programs are compiled too (Iris/OptiFine
    // quote through `ftransform`, `gl_TextureMatrix` and `gl_MultiTexCoord0`).
    for vertex in [
        "deferred.vsh",
        "composite.vsh",
        "gbuffers_terrain.vsh",
        "gbuffers_water.vsh",
    ] {
        compile_vertex_source(gl, &pack.shader_text(vertex)?, vertex)?;
    }
    for fragment in ["gbuffers_terrain.fsh", "gbuffers_water.fsh"] {
        compile_fragment_source(gl, &pack.shader_text(fragment)?, fragment)?;
    }

    let aspect = width as f32 / height as f32;
    let projection = Mat4::perspective_rh_gl(args.fov_y_deg.to_radians(), aspect, 0.05, 1.0e6);
    let projection_inverse = projection.inverse();
    // World-from-view rotation for yaw = pitch = 0: view forward is -Z.
    let model_view_inverse = Mat4::from_cols_array(&[
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, -1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ]);
    let projection_inverse_array = projection_inverse.to_cols_array();
    let model_view_inverse_array = model_view_inverse.to_cols_array();

    let pass_through = check_pass_through(
        gl,
        &quad,
        &composite_display,
        &display_fbo,
        &scene,
        &depth_partial,
        &depth_one,
        width,
        height,
        &projection_inverse_array,
        &model_view_inverse_array,
    )?;

    let mut scenarios = Vec::new();
    for &elevation in &elevations {
        let sun_world = sun_direction(elevation, 0.0);
        let view = View {
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            fov_y_deg: args.fov_y_deg,
            sun_azimuth_deg: 0.0,
            sun_elevation_deg: elevation,
            altitude_km: args.altitude_km,
        };
        let reference_no_disk = reference_radiance(&mut reference, &device, &queue, view, false)?;
        let reference_disk = reference_radiance(&mut reference, &device, &queue, view, true)?;

        // ---- OpenGL SkyView rebuild + composite.
        let luniforms = LutUniforms {
            sun_position: [
                sun_world[0] * 100.0,
                sun_world[1] * 100.0,
                sun_world[2] * 100.0,
            ],
            eye_altitude: args.altitude_km * 1000.0,
        };
        let (deferred_ms, _) = benchmark(|| {
            render_lut(
                gl, &quad, &deferred, &lut_fbo, &tau, &atlas, &source, &luniforms,
            );
        });
        let pack_no_disk = render_composite_linear(
            gl,
            &quad,
            &composite_linear_no_disk,
            &linear_fbo,
            &black_scene,
            &tau,
            &lut_fbo.color,
            &depth_one,
            &luniforms,
            &projection_inverse_array,
            &model_view_inverse_array,
            width,
            height,
            false,
        );
        let pack_disk = render_composite_linear(
            gl,
            &quad,
            &composite_linear_disk,
            &linear_fbo,
            &black_scene,
            &tau,
            &lut_fbo.color,
            &depth_one,
            &luniforms,
            &projection_inverse_array,
            &model_view_inverse_array,
            width,
            height,
            true,
        );
        let (composite_ms, _) = benchmark(|| {
            render_composite_linear(
                gl,
                &quad,
                &composite_linear_disk,
                &linear_fbo,
                &black_scene,
                &tau,
                &lut_fbo.color,
                &depth_one,
                &luniforms,
                &projection_inverse_array,
                &model_view_inverse_array,
                width,
                height,
                true,
            );
        });
        let pack_display = render_composite_display(
            gl,
            &quad,
            &composite_display,
            &display_fbo,
            &black_scene,
            &tau,
            &lut_fbo.color,
            &depth_one,
            &luniforms,
            &projection_inverse_array,
            &model_view_inverse_array,
            width,
            height,
        );

        let pack_no_disk_top = flip_rows_f32(&pack_no_disk, width, height);
        let pack_disk_top = flip_rows_f32(&pack_disk, width, height);
        let metrics = compare_linear(&pack_no_disk_top, &reference_no_disk, width, height);
        let mask = solar_disk_mask(width, height, args.fov_y_deg, elevation);
        let disk_ratio = disk_radiance_ratio(
            &pack_disk_top,
            &pack_no_disk_top,
            &reference_disk,
            &reference_no_disk,
            &mask,
        );

        let mut images = Vec::new();
        if !args.metrics_only {
            let tag = format!("sun_{:+.0}", elevation);
            let pack_display_top = flip_rows(&pack_display, width, height);
            let pack_path = args.out.join(format!("{tag}_pack.png"));
            save_rgba8_png(&pack_path, width, height, &pack_display_top)?;
            images.push(file_name(&pack_path));

            let reference_display = display_linear(
                gl,
                &quad,
                &display,
                &display_fbo,
                &reference_disk,
                width,
                height,
                args.exposure,
            );
            let reference_top = flip_rows(&reference_display, width, height);
            let reference_path = args.out.join(format!("{tag}_reference.png"));
            save_rgba8_png(&reference_path, width, height, &reference_top)?;
            images.push(file_name(&reference_path));

            let compare_path = args.out.join(format!("{tag}_compare.png"));
            save_side_by_side(
                &compare_path,
                width,
                height,
                &reference_top,
                &pack_display_top,
            )?;
            images.push(file_name(&compare_path));
        }

        scenarios.push(ScenarioReport {
            sun_elevation_deg: elevation,
            sun_azimuth_deg: 0.0,
            linear_radiance_difference: metrics,
            solar_disk_ratio: disk_ratio,
            sky_view_build_ms: deferred_ms,
            composite_ms,
            images,
        });
    }

    Ok(HarnessReport {
        kind: "sky_tracer_pack_validation_v1",
        gl_renderer: context.renderer_string(),
        pack: args.pack.display().to_string(),
        width,
        height,
        fov_y_deg: args.fov_y_deg,
        observer_altitude_km: args.altitude_km,
        runtime_steps: pack.runtime_steps,
        reference: "sky-realtime render pipeline (use_sky_view = false, direct 4-wave integration)",
        scenarios,
        pass_through,
        generated_by: concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION")),
    })
}

struct LutUniforms {
    sun_position: [f32; 3],
    eye_altitude: f32,
}

#[allow(clippy::too_many_arguments)]
fn render_lut(
    gl: &glow::Context,
    quad: &FullscreenQuad,
    program: &GlProgram,
    target: &GlFramebuffer,
    tau: &GlTexture,
    atlas: &GlTexture,
    source: &GlTexture,
    uniforms: &LutUniforms,
) {
    target.bind(gl);
    unsafe {
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
    }
    program.use_program(gl);
    program.uniform_vec3(gl, "sunPosition", uniforms.sun_position);
    program.uniform_f32(gl, "eyeAltitude", uniforms.eye_altitude);
    program.uniform_i32(gl, "dimension", 0);
    // Deferred uses an identity model-view: sunPosition already is world space.
    program.uniform_mat4(gl, "gbufferModelViewInverse", &IDENTITY);
    bind_sampler(gl, program, "colortex4", 0, tau);
    bind_sampler(gl, program, "colortex6", 1, atlas);
    bind_sampler(gl, program, "colortex7", 2, source);
    quad.draw(gl);
    unsafe {
        gl.finish();
    }
}

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

#[allow(clippy::too_many_arguments)]
fn render_composite_linear(
    gl: &glow::Context,
    quad: &FullscreenQuad,
    program: &GlProgram,
    target: &GlFramebuffer,
    scene: &GlTexture,
    tau: &GlTexture,
    sky_lut: &GlTexture,
    depth: &GlTexture,
    uniforms: &LutUniforms,
    projection_inverse: &[f32; 16],
    model_view_inverse: &[f32; 16],
    width: u32,
    height: u32,
    _with_disk: bool,
) -> Vec<f32> {
    target.bind(gl);
    unsafe {
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
    }
    program.use_program(gl);
    // Composite receives the Sun in view space; model-view is a Z flip here.
    let sun = uniforms.sun_position;
    program.uniform_vec3(gl, "sunPosition", [sun[0], sun[1], -sun[2]]);
    program.uniform_f32(gl, "eyeAltitude", uniforms.eye_altitude);
    program.uniform_f32(gl, "rainStrength", 0.0);
    program.uniform_i32(gl, "dimension", 0);
    program.uniform_i32(gl, "isEyeInWater", 0);
    program.uniform_mat4(gl, "gbufferProjectionInverse", projection_inverse);
    program.uniform_mat4(gl, "gbufferModelViewInverse", model_view_inverse);
    bind_sampler(gl, program, "colortex0", 0, scene);
    bind_sampler(gl, program, "colortex4", 1, tau);
    bind_sampler(gl, program, "colortex5", 2, sky_lut);
    bind_sampler(gl, program, "depthtex0", 3, depth);
    quad.draw(gl);
    unsafe {
        gl.finish();
    }
    let _ = (width, height);
    target.read_rgba_f32(gl)
}

#[allow(clippy::too_many_arguments)]
fn render_composite_display(
    gl: &glow::Context,
    quad: &FullscreenQuad,
    program: &GlProgram,
    target: &GlFramebuffer,
    scene: &GlTexture,
    tau: &GlTexture,
    sky_lut: &GlTexture,
    depth: &GlTexture,
    uniforms: &LutUniforms,
    projection_inverse: &[f32; 16],
    model_view_inverse: &[f32; 16],
    width: u32,
    height: u32,
) -> Vec<u8> {
    target.bind(gl);
    unsafe {
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
    }
    program.use_program(gl);
    let sun = uniforms.sun_position;
    program.uniform_vec3(gl, "sunPosition", [sun[0], sun[1], -sun[2]]);
    program.uniform_f32(gl, "eyeAltitude", uniforms.eye_altitude);
    program.uniform_f32(gl, "rainStrength", 0.0);
    program.uniform_i32(gl, "dimension", 0);
    program.uniform_i32(gl, "isEyeInWater", 0);
    program.uniform_mat4(gl, "gbufferProjectionInverse", projection_inverse);
    program.uniform_mat4(gl, "gbufferModelViewInverse", model_view_inverse);
    bind_sampler(gl, program, "colortex0", 0, scene);
    bind_sampler(gl, program, "colortex4", 1, tau);
    bind_sampler(gl, program, "colortex5", 2, sky_lut);
    bind_sampler(gl, program, "depthtex0", 3, depth);
    quad.draw(gl);
    unsafe {
        gl.finish();
    }
    let _ = (width, height);
    target.read_rgba8(gl)
}

fn display_linear(
    gl: &glow::Context,
    quad: &FullscreenQuad,
    program: &GlProgram,
    target: &GlFramebuffer,
    source: &[f32],
    width: u32,
    height: u32,
    exposure: f32,
) -> Vec<u8> {
    // The reference readback is top-down; GL texture row 0 is sampled by the
    // fullscreen quad at the framebuffer bottom, so upload the bottom-up image
    // and let the final row flip in save_rgba8_png restore top-down order.
    let flipped = flip_rows_f32(source, width, height);
    let texture = texture_rgba32f(gl, width, height, Some(&flipped));
    target.bind(gl);
    unsafe {
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
    }
    program.use_program(gl);
    program.uniform_f32(gl, "exposure", exposure);
    bind_sampler(gl, program, "source", 0, &texture);
    quad.draw(gl);
    unsafe {
        gl.finish();
    }
    target.read_rgba8(gl)
}

fn benchmark(mut render: impl FnMut()) -> (f64, ()) {
    render(); // warm up pipelines / driver caches
    let mut best = f64::INFINITY;
    for _ in 0..3 {
        let start = std::time::Instant::now();
        render();
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        best = best.min(elapsed);
    }
    (best, ())
}

fn sun_direction(elevation_deg: f32, azimuth_deg: f32) -> [f32; 3] {
    let elevation = elevation_deg.to_radians();
    let azimuth = azimuth_deg.to_radians();
    [
        azimuth.sin() * elevation.cos(),
        elevation.sin(),
        azimuth.cos() * elevation.cos(),
    ]
}

fn camera_ray(x: u32, y: u32, width: u32, height: u32, fov_y_deg: f32) -> [f32; 3] {
    let fov = fov_y_deg.to_radians();
    let tan_half = (fov * 0.5).tan();
    let aspect = width as f32 / height as f32;
    let ux = (x as f32 + 0.5) / width as f32 * 2.0 - 1.0;
    let uy = (y as f32 + 0.5) / height as f32 * 2.0 - 1.0;
    // Camera: yaw = pitch = 0, world forward = +Z, right = +X, up = +Y.
    let direction = [ux * tan_half * aspect, -uy * tan_half, 1.0];
    let length =
        (direction[0] * direction[0] + direction[1] * direction[1] + direction[2] * direction[2])
            .sqrt();
    [
        direction[0] / length,
        direction[1] / length,
        direction[2] / length,
    ]
}

fn solar_disk_mask(width: u32, height: u32, fov_y_deg: f32, elevation_deg: f32) -> Vec<bool> {
    let sun = sun_direction(elevation_deg, 0.0);
    let cos_limit = 1.2f32.to_radians().cos();
    let mut mask = vec![false; (width * height) as usize];
    for y in 0..height {
        for x in 0..width {
            let ray = camera_ray(x, y, width, height, fov_y_deg);
            let cosine = ray[0] * sun[0] + ray[1] * sun[1] + ray[2] * sun[2];
            mask[(y * width + x) as usize] = cosine >= cos_limit;
        }
    }
    mask
}

fn luminance(rgb: &[f32]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

fn compare_linear(pack: &[f32], reference: &[f32], _width: u32, _height: u32) -> Metrics {
    let pixels = pack.len() / 4;
    let mut relative = Vec::with_capacity(pixels);
    let mut sum = 0.0f64;
    let mut max = 0.0f64;
    for i in 0..pixels {
        let pack_luminance = luminance(&pack[i * 4..i * 4 + 3]) as f64;
        let reference_luminance = luminance(&reference[i * 4..i * 4 + 3]) as f64;
        let value =
            (pack_luminance - reference_luminance).abs() / reference_luminance.abs().max(1e-9);
        relative.push(value);
        sum += value;
        max = max.max(value);
    }
    relative.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let percentile = |p: f64| -> f64 {
        if relative.is_empty() {
            return 0.0;
        }
        let index = ((relative.len() as f64 - 1.0) * p).round() as usize;
        relative[index]
    };
    Metrics {
        mean_relative_difference: sum / pixels.max(1) as f64,
        p50: percentile(0.50),
        p95: percentile(0.95),
        p99: percentile(0.99),
        max,
        pixels,
    }
}

fn disk_radiance_ratio(
    pack_disk: &[f32],
    pack_no_disk: &[f32],
    reference_disk: &[f32],
    reference_no_disk: &[f32],
    mask: &[bool],
) -> Option<f64> {
    let mut numerator = 0.0f64;
    let mut denominator = 0.0f64;
    let mut covered = 0usize;
    for i in 0..mask.len() {
        if !mask[i] {
            continue;
        }
        covered += 1;
        let pack_delta = luminance(&pack_disk[i * 4..i * 4 + 3]) as f64
            - luminance(&pack_no_disk[i * 4..i * 4 + 3]) as f64;
        let reference_delta = luminance(&reference_disk[i * 4..i * 4 + 3]) as f64
            - luminance(&reference_no_disk[i * 4..i * 4 + 3]) as f64;
        numerator += pack_delta;
        denominator += reference_delta;
    }
    if covered == 0 || denominator.abs() < 1e-6 {
        return None;
    }
    Some(numerator / denominator)
}

fn reference_radiance(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    view: View,
    include_disk: bool,
) -> Result<Vec<f32>, String> {
    renderer.use_sky_view = false;
    renderer.spectral_output = false;
    renderer.segment_km = 0.0;
    renderer.include_sun_disk = include_disk;
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(queue, &mut encoder, view);
    queue.submit([encoder.finish()]);
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| format!("poll: {e}"))?;
    let bytes = sky_realtime::read_texture(device, queue, renderer.target_texture(), 16)
        .map_err(|e| e.to_string())?;
    Ok(bytemuck::cast_slice(&bytes).to_vec())
}

fn flip_rows(data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let row_bytes = width as usize * 4;
    let mut out = vec![0u8; data.len()];
    for row in 0..height as usize {
        let source = row * row_bytes;
        let target = (height as usize - 1 - row) * row_bytes;
        out[target..target + row_bytes].copy_from_slice(&data[source..source + row_bytes]);
    }
    out
}

fn flip_rows_f32(data: &[f32], width: u32, height: u32) -> Vec<f32> {
    let row_values = width as usize * 4;
    let mut out = vec![0.0f32; data.len()];
    for row in 0..height as usize {
        let source = row * row_values;
        let target = (height as usize - 1 - row) * row_values;
        out[target..target + row_values].copy_from_slice(&data[source..source + row_values]);
    }
    out
}

fn make_scene_texture(gl: &glow::Context, width: u32, height: u32) -> GlTexture {
    let mut data = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let index = ((y * width + x) * 4) as usize;
            data[index] = (x * 255 / (width - 1)) as u8;
            data[index + 1] = (y * 255 / (height - 1)) as u8;
            data[index + 2] = 128;
            data[index + 3] = 255;
        }
    }
    texture_rgba8(gl, width, height, Some(&data))
}

#[allow(clippy::too_many_arguments)]
fn check_pass_through(
    gl: &glow::Context,
    quad: &FullscreenQuad,
    program: &GlProgram,
    target: &GlFramebuffer,
    scene: &GlTexture,
    depth_partial: &GlTexture,
    depth_one: &GlTexture,
    width: u32,
    height: u32,
    projection_inverse: &[f32; 16],
    model_view_inverse: &[f32; 16],
) -> Result<PassThroughReport, String> {
    // Geometry half: depth < 1 must be copied through untouched.
    target.bind(gl);
    unsafe {
        gl.clear_color(1.0, 0.0, 1.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
    }
    program.use_program(gl);
    program.uniform_vec3(gl, "sunPosition", [0.0, 100.0, 0.0]);
    program.uniform_f32(gl, "eyeAltitude", 70.0);
    program.uniform_f32(gl, "rainStrength", 0.0);
    program.uniform_i32(gl, "dimension", 0);
    program.uniform_i32(gl, "isEyeInWater", 0);
    program.uniform_mat4(gl, "gbufferProjectionInverse", projection_inverse);
    program.uniform_mat4(gl, "gbufferModelViewInverse", model_view_inverse);
    bind_sampler(gl, program, "colortex0", 0, scene);
    bind_sampler(gl, program, "colortex4", 1, depth_one); // not read on this path
    bind_sampler(gl, program, "colortex5", 2, depth_one);
    bind_sampler(gl, program, "depthtex0", 3, depth_partial);
    quad.draw(gl);
    unsafe {
        gl.finish();
    }
    let depth_masked = target.read_rgba8(gl);
    let scene_bytes = scene_data(width, height);
    let mut max_depth_difference = 0i32;
    for row in 0..height / 2 {
        for x in 0..width {
            let index = ((row * width + x) * 4) as usize;
            for channel in 0..4 {
                let difference = (depth_masked[index + channel] as i32
                    - scene_bytes[index + channel] as i32)
                    .abs();
                max_depth_difference = max_depth_difference.max(difference);
            }
        }
    }

    // Other dimensions: the composite must leave the whole frame alone.
    target.bind(gl);
    unsafe {
        gl.clear_color(1.0, 0.0, 1.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
    }
    program.use_program(gl);
    program.uniform_i32(gl, "dimension", -1);
    bind_sampler(gl, program, "depthtex0", 3, depth_one);
    quad.draw(gl);
    unsafe {
        gl.finish();
    }
    let dimension_bytes = target.read_rgba8(gl);
    let mut max_dimension_difference = 0i32;
    for row in 0..height {
        for x in 0..width {
            let index = ((row * width + x) * 4) as usize;
            for channel in 0..4 {
                let difference = (dimension_bytes[index + channel] as i32
                    - scene_bytes[index + channel] as i32)
                    .abs();
                max_dimension_difference = max_dimension_difference.max(difference);
            }
        }
    }

    Ok(PassThroughReport {
        depth_masked_pixels_unchanged: max_depth_difference == 0,
        other_dimension_pixels_unchanged: max_dimension_difference == 0,
        max_byte_difference: max_depth_difference.max(max_dimension_difference),
    })
}

fn scene_data(width: u32, height: u32) -> Vec<u8> {
    let mut data = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let index = ((y * width + x) * 4) as usize;
            data[index] = (x * 255 / (width - 1)) as u8;
            data[index + 1] = (y * 255 / (height - 1)) as u8;
            data[index + 2] = 128;
            data[index + 3] = 255;
        }
    }
    data
}

fn save_rgba8_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for pixel in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&pixel[..3]);
    }
    image::save_buffer(path, &rgb, width, height, image::ColorType::Rgb8)
        .map_err(|e| format!("save {}: {e}", path.display()))
}

fn save_side_by_side(
    path: &Path,
    width: u32,
    height: u32,
    left: &[u8],
    right: &[u8],
) -> Result<(), String> {
    let gap = 8u32;
    let total_width = width * 2 + gap;
    let mut image = vec![0u8; (total_width * height * 3) as usize];
    for y in 0..height {
        for x in 0..width {
            let source = ((y * width + x) * 4) as usize;
            let target_left = ((y * total_width + x) * 3) as usize;
            let target_right = ((y * total_width + width + gap + x) * 3) as usize;
            image[target_left..target_left + 3].copy_from_slice(&left[source..source + 3]);
            image[target_right..target_right + 3].copy_from_slice(&right[source..source + 3]);
        }
    }
    image::save_buffer(path, &image, total_width, height, image::ColorType::Rgb8)
        .map_err(|e| format!("save {}: {e}", path.display()))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

fn parse_elevations(text: &str) -> Result<Vec<f32>, String> {
    let mut values = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let value: f32 = part
            .parse()
            .map_err(|e| format!("sun elevation '{part}': {e}"))?;
        if !(-90.0..=90.0).contains(&value) {
            return Err(format!("sun elevation {value} outside [-90, 90]"));
        }
        values.push(value);
    }
    if values.is_empty() {
        return Err("no sun elevations given".into());
    }
    Ok(values)
}
