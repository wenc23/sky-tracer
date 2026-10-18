//! wgpu compute renderer for the stateless screen-space weather effect.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::{Config, Input, Result, Stats};

/// The full-screen compute shader. It is compiled with the crate so the effect
/// cannot drift from the renderer or be replaced by an application shader.
pub const SHADER: &str = include_str!("weather.wgsl");

pub fn shader_source() -> &'static str {
    SHADER
}

const WORKGROUP_SIZE: u32 = 8;

/// Uniform block shared with `weather.wgsl`. Every member is a `vec4`, matching
/// the WGSL layout exactly; `size_of::<Params>() == 176` is asserted in tests.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Params {
    camera_origin: [f32; 4],
    camera_forward: [f32; 4],
    camera_right: [f32; 4],
    camera_up: [f32; 4],
    sun_direction: [f32; 4],
    wind: [f32; 4],
    weather: [f32; 4],
    atmosphere: [f32; 4],
    lightning: [f32; 4],
    counts: [u32; 4],
    viewport: [u32; 4],
}

fn build_params(
    config: &Config,
    input: &crate::NormalizedInput,
    size: [u32; 2],
    fov_y_deg: f32,
) -> Params {
    let tan_half = (fov_y_deg * 0.5).to_radians().tan();
    Params {
        camera_origin: [
            input.origin_m[0],
            input.origin_m[1],
            input.origin_m[2],
            input.time_s,
        ],
        camera_forward: [
            input.forward[0],
            input.forward[1],
            input.forward[2],
            tan_half,
        ],
        camera_right: [input.right[0], input.right[1], input.right[2], 0.0],
        camera_up: [input.up[0], input.up[1], input.up[2], 0.0],
        sun_direction: [
            input.sun_direction[0],
            input.sun_direction[1],
            input.sun_direction[2],
            0.0,
        ],
        wind: [input.wind[0], input.wind[1], 0.0, 0.0],
        weather: input.intensities,
        atmosphere: input.atmosphere,
        lightning: [input.lightning_flash, 0.0, 0.0, 0.0],
        counts: config.layer_counts(),
        viewport: [
            size[0],
            size[1],
            config.bolt_segments(),
            input.lightning_seed,
        ],
    }
}

/// Screen-space weather effect renderer.
///
/// The renderer owns an `Rgba32Float` output texture the size of the viewport.
/// One compute dispatch composites every enabled effect over the background.
pub struct Renderer {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    output: wgpu::Texture,
    output_view: wgpu::TextureView,
    background: wgpu::TextureView,
    #[allow(dead_code)]
    background_default: wgpu::Texture,
    bind_group: Option<wgpu::BindGroup>,
    background_usable: bool,
    size: [u32; 2],
    config: Config,
    stats: Stats,
}

impl Renderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, config: Config) -> Result<Self> {
        config.validate()?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("weather effects shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("weather effects bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("weather effects pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("weather effects pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("weather effect parameters"),
            contents: bytemuck::bytes_of(&Params::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let (background_default, background) = create_default_background(device);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &background_default,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&[0.0f32, 0.0, 0.0, 1.0]),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(16),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let (output, output_view) = create_output(device, [1, 1]);
        let bind_group = create_bind_group(device, &layout, &params, &background, &output_view);
        Ok(Self {
            pipeline,
            layout,
            params,
            output,
            output_view,
            background,
            background_default,
            bind_group: Some(bind_group),
            background_usable: true,
            size: [1, 1],
            config,
            stats: Stats::default(),
        })
    }

    /// Recreate the output target when the viewport size changes. Returns
    /// whether the size changed.
    pub fn resize(&mut self, device: &wgpu::Device, size: [u32; 2]) -> bool {
        let size = [size[0].max(1), size[1].max(1)];
        if size == self.size {
            return false;
        }
        self.size = size;
        let (output, output_view) = create_output(device, size);
        self.output = output;
        self.output_view = output_view;
        if self.background_usable {
            self.bind_group = Some(create_bind_group(
                device,
                &self.layout,
                &self.params,
                &self.background,
                &self.output_view,
            ));
        }
        self.stats.last_output = size;
        true
    }

    /// Bind the linear sky background. The view must be a float-sample texture
    /// (`Rgba32Float` or `Rgba16Float`), because the shader reads it with
    /// `textureLoad`. A non-float view is recorded as unusable and `render`
    /// reports an error instead of panicking.
    pub fn set_background(&mut self, device: &wgpu::Device, background: &wgpu::TextureView) {
        let usable = background
            .texture()
            .format()
            .sample_type(None, None)
            .is_some_and(|sample_type| {
                matches!(sample_type, wgpu::TextureSampleType::Float { .. })
            });
        self.background_usable = usable;
        if usable {
            let view = background.clone();
            let bind_group =
                create_bind_group(device, &self.layout, &self.params, &view, &self.output_view);
            self.background = view;
            self.bind_group = Some(bind_group);
        }
    }

    /// Encode one full-screen weather composite. The output target receives
    /// `background` unchanged when every weather intensity is zero.
    pub fn render(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        input: Input,
    ) -> Result<()> {
        if !self.background_usable {
            return Err(
                "set_background requires a float-sample texture view (e.g. Rgba32Float)".into(),
            );
        }
        let normalized = input.normalize()?;
        let params = build_params(&self.config, &normalized, self.size, input.camera.fov_y_deg);
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        let [width, height] = self.size;
        let bind_group = self
            .bind_group
            .as_ref()
            .ok_or("weather effect bind group is not initialized")?;
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("weather effect composite"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.dispatch_workgroups(
                width.div_ceil(WORKGROUP_SIZE),
                height.div_ceil(WORKGROUP_SIZE),
                1,
            );
        }
        self.stats.frames += 1;
        self.stats.last_output = self.size;
        Ok(())
    }

    /// Linear Rec.2020 weather-composited output for the current frame.
    pub fn target_view(&self) -> &wgpu::TextureView {
        &self.output_view
    }

    /// Output texture accessor for readback and auditing.
    pub fn target_texture(&self) -> &wgpu::Texture {
        &self.output
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn create_default_background(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("weather default background"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
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

fn create_output(device: &wgpu::Device, size: [u32; 2]) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("weather effect output"),
        size: wgpu::Extent3d {
            width: size[0].max(1),
            height: size[1].max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    (texture, view)
}

fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    params: &wgpu::Buffer,
    background: &wgpu::TextureView,
    output: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("weather effect bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(background),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(output),
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Camera, Sun};

    fn zero_input() -> Input {
        Input {
            time_s: 0.0,
            camera: Camera {
                origin_m: [10.0, 20.0, 30.0],
                yaw_deg: 10.0,
                pitch_deg: 5.0,
                fov_y_deg: 60.0,
            },
            sun: Sun {
                direction: [0.3, 0.7, 0.1],
            },
            rain: 0.0,
            snow: 0.0,
            hail: 0.0,
            sleet: 0.0,
            fog: 0.0,
            dust: 0.0,
            wind: [2.0, -1.0],
            cloud_darkening: 0.0,
            rainbow: 0.0,
            lightning_flash: 0.0,
            lightning_seed: 42,
        }
    }

    #[test]
    fn params_layout_matches_wgsl_uniform_layout() {
        assert_eq!(std::mem::size_of::<Params>(), 176);
        assert_eq!(std::mem::size_of::<Params>() % 16, 0);
        assert_eq!(std::mem::align_of::<Params>(), 4);
    }

    #[test]
    fn zero_input_builds_zero_effect_uniforms() {
        let config = Config::default();
        let input = zero_input();
        let normalized = input.normalize().unwrap();
        let params = build_params(&config, &normalized, [640, 360], 60.0);
        assert_eq!(params.weather, [0.0; 4]);
        assert_eq!(params.atmosphere, [0.0; 4]);
        assert_eq!(params.lightning, [0.0; 4]);
        assert_eq!(params.counts, config.layer_counts());
        assert_eq!(params.viewport, [640, 360, config.bolt_segments(), 42]);
    }

    #[test]
    fn build_params_copies_camera_and_sun() {
        let config = Config::default();
        let input = zero_input();
        let normalized = input.normalize().unwrap();
        let params = build_params(&config, &normalized, [7, 5], 90.0);
        assert_eq!(params.camera_origin, [10.0, 20.0, 30.0, 0.0]);
        assert!((params.camera_forward[3] - 1.0).abs() < 1.0e-5); // tan(45 degrees)
        let length = (params.sun_direction[0] * params.sun_direction[0]
            + params.sun_direction[1] * params.sun_direction[1]
            + params.sun_direction[2] * params.sun_direction[2])
            .sqrt();
        assert!((length - 1.0).abs() < 1.0e-6);
        assert_eq!(params.viewport[0], 7);
        assert_eq!(params.viewport[1], 5);
    }

    #[test]
    fn shader_is_utf8_without_bom() {
        let bytes = include_bytes!("weather.wgsl");
        assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
        let source = std::str::from_utf8(bytes).expect("weather.wgsl must be valid UTF-8");
        assert!(source.contains("fn main("));
        assert!(source.contains("texture_storage_2d<rgba32float, write>"));
    }

    #[test]
    fn shader_parses_validates_and_emits_spirv() {
        let source = shader_source();
        let module = naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
        let spv =
            naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
                .unwrap();
        assert!(!spv.is_empty());
    }

    #[test]
    fn shader_has_one_full_screen_entry_point() {
        let module = naga::front::wgsl::parse_str(shader_source()).unwrap();
        assert_eq!(module.entry_points.len(), 1);
        let entry = &module.entry_points[0];
        assert_eq!(entry.name, "main");
        assert_eq!(entry.workgroup_size, [8, 8, 1]);
    }
}
