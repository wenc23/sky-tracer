//! wgpu real-time volumetric cloud renderer.

use bytemuck::{Pod, Zeroable};
use half::f16;
use wgpu::util::DeviceExt;

use crate::{Camera, Config, Result, Stats, Sun, VolumeDescriptor, optics};

pub const SHADER: &str = include_str!("clouds.wgsl");
pub const COMPOSITE_SHADER: &str = include_str!("composite.wgsl");
pub const RESOLVE_SHADER: &str = include_str!("resolve.wgsl");
pub const SHADOW_SHADER: &str = include_str!("shadow.wgsl");

pub fn shader_source() -> &'static str {
    SHADER
}

pub fn composite_shader_source() -> &'static str {
    COMPOSITE_SHADER
}

pub fn resolve_shader_source() -> &'static str {
    RESOLVE_SHADER
}

pub fn shadow_shader_source() -> &'static str {
    SHADOW_SHADER
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Params {
    camera_origin: [f32; 4],
    camera_forward: [f32; 4],
    camera_right: [f32; 4],
    camera_up: [f32; 4],
    volume_origin: [f32; 4],
    volume_spacing: [f32; 4],
    volume_dims: [u32; 4],
    sun_dir: [f32; 4],
    solar_weights: [[f32; 4]; 3],
    viewport: [f32; 4],
    params: [f32; 4],
    /// x = detail erosion strength; yz = world-space drift of the detail field
    /// (matches the volume's wind advection); w = sun shadow march steps.
    detail: [f32; 4],
    /// x = aerial-perspective e-folding distance (0 disables), y = strength.
    /// z/w = visual base-volume drift (metres, x/z) between weather-volume
    /// uploads; the shader shifts the volume sampling by it so cloud motion is
    /// continuous at frame rate instead of stepping with the rebuild cadence.
    aerial: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct TemporalParams {
    camera_forward: [f32; 4],
    camera_right: [f32; 4],
    camera_up: [f32; 4],
    prev_forward: [f32; 4],
    prev_right: [f32; 4],
    prev_up: [f32; 4],
    /// x = tan(fov_y/2), y = aspect, z = history_valid, w = blend alpha.
    data: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct ShadowParams {
    sun: [f32; 4],
    volume_origin: [f32; 4],
    volume_spacing: [f32; 4],
    volume_dims: [u32; 4],
    /// xz origin, xz extent of the shadow domain (normally the volume box).
    domain: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct CompositeParams {
    camera_origin: [f32; 4],
    camera_forward: [f32; 4],
    camera_right: [f32; 4],
    camera_up: [f32; 4],
    volume_domain: [f32; 4],
    /// x = tan(fov_y/2), y = aspect, z = cloud shadow strength, w unused.
    data: [f32; 4],
}

pub struct Renderer {
    config: Config,
    pipeline: wgpu::ComputePipeline,
    composite_pipeline: wgpu::ComputePipeline,
    resolve_pipeline: wgpu::ComputePipeline,
    shadow_pipeline: wgpu::ComputePipeline,
    layout0: wgpu::BindGroupLayout,
    layout1: wgpu::BindGroupLayout,
    layout2: wgpu::BindGroupLayout,
    resolve_layout: wgpu::BindGroupLayout,
    #[allow(dead_code)]
    shadow_layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    temporal: wgpu::Buffer,
    shadow_params: wgpu::Buffer,
    composite_params: wgpu::Buffer,
    #[allow(dead_code)]
    shadow_texture: wgpu::Texture,
    shadow_view: wgpu::TextureView,
    shadow_group: wgpu::BindGroup,
    shadow_valid: bool,
    shadow_dirty: bool,
    shadow_sun: [f32; 3],
    /// One `vec4`: x = overcast occlusion in [0, 1]. Shared by the volume and
    /// composite passes; the composite pass dims the sky/ground background and
    /// the cloud radiance as the weather gets more overcast.
    atmosphere: wgpu::Buffer,
    overcast: f32,
    storm_severity: f32,
    volume: wgpu::Texture,
    volume_view: wgpu::TextureView,
    volume_sampler: wgpu::Sampler,
    /// Small tileable 3D noise used to erode the low-frequency density field
    /// with sub-voxel detail (R = 8 km tile, G = 2 km tile). This is a visual
    /// sub-grid turbulence model, not extra thermodynamically conserved water.
    #[allow(dead_code)]
    detail_texture: wgpu::Texture,
    detail_view: wgpu::TextureView,
    detail_sampler: wgpu::Sampler,
    detail_drift: [f32; 2],
    detail_drift_delta: f32,
    /// Visual base-volume drift in metres (x, z). The weather volume is only
    /// rebuilt every few hundred milliseconds; this offset interpolates the
    /// wind advection between rebuilds so the cloud *motion* is continuous at
    /// frame rate instead of stepping at the upload cadence.
    volume_drift: [f32; 2],
    /// Raw per-frame trace result (radiance + transmittance).
    cloud_buffer: wgpu::Texture,
    cloud_view: wgpu::TextureView,
    /// Temporally accumulated trace result; this is what the composite pass
    /// upsamples.
    resolved: wgpu::Texture,
    resolved_view: wgpu::TextureView,
    /// Previous frame's resolved trace, read by the resolve pass.
    history: wgpu::Texture,
    history_view: wgpu::TextureView,
    output: wgpu::Texture,
    output_view: wgpu::TextureView,
    main_group: wgpu::BindGroup,
    environment_group: wgpu::BindGroup,
    composite_group: wgpu::BindGroup,
    resolve_group: wgpu::BindGroup,
    // Default bindings keep a valid group alive before the application binds
    // its own environment/background views. They are also reused on resize.
    #[allow(dead_code)]
    environment_default: wgpu::Texture,
    #[allow(dead_code)]
    sun_default: wgpu::Texture,
    background_default: wgpu::Texture,
    solar_weights: [[f32; 4]; 3],
    size: [u32; 2],
    stats: Stats,
    volume_descriptor: Option<VolumeDescriptor>,
    volume_uploaded: bool,
    /// Camera basis and tan(fov/2) of the previous rendered frame, used to
    /// reproject the temporal history.
    prev_basis: ([f32; 3], [f32; 3], [f32; 3]),
    prev_tan_half: f32,
    prev_origin: [f32; 3],
    history_valid: bool,
    last_sun_dir: [f32; 3],
    last_overcast: f32,
    last_storm_severity: f32,
}

impl Renderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, config: Config) -> Result<Self> {
        config.validate()?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cloud volume shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cloud main bind group layout"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cloud environment bind group layout"),
            entries: &[texture_entry(0), texture_entry(1)],
        });
        let layout2 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cloud composite bind group layout"),
            entries: &[
                texture_entry(0),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cloud pipeline layout"),
            bind_group_layouts: &[Some(&layout0), Some(&layout1)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("cloud volume pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cloud composite shader"),
            source: wgpu::ShaderSource::Wgsl(COMPOSITE_SHADER.into()),
        });
        let composite_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cloud composite pipeline layout"),
            bind_group_layouts: &[Some(&layout2)],
            immediate_size: 0,
        });
        let composite_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("cloud composite pipeline"),
            layout: Some(&composite_layout),
            module: &composite_shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let resolve_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cloud temporal resolve shader"),
            source: wgpu::ShaderSource::Wgsl(RESOLVE_SHADER.into()),
        });
        let resolve_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cloud resolve bind group layout"),
            entries: &[
                texture_entry(0),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let resolve_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("cloud resolve pipeline layout"),
                bind_group_layouts: &[Some(&resolve_layout)],
                immediate_size: 0,
            });
        let resolve_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("cloud resolve pipeline"),
            layout: Some(&resolve_pipeline_layout),
            module: &resolve_shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let shadow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cloud shadow shader"),
            source: wgpu::ShaderSource::Wgsl(SHADOW_SHADER.into()),
        });
        let shadow_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cloud shadow bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::R32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let shadow_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("cloud shadow pipeline layout"),
                bind_group_layouts: &[Some(&shadow_layout)],
                immediate_size: 0,
            });
        let shadow_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("cloud shadow pipeline"),
            layout: Some(&shadow_pipeline_layout),
            module: &shadow_shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cloud parameters"),
            contents: bytemuck::bytes_of(&Params::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let temporal = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cloud temporal parameters"),
            contents: bytemuck::bytes_of(&TemporalParams::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let shadow_params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cloud shadow parameters"),
            contents: bytemuck::bytes_of(&ShadowParams::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let composite_params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cloud composite parameters"),
            contents: bytemuck::bytes_of(&CompositeParams::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let atmosphere = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cloud atmosphere parameters"),
            contents: bytemuck::bytes_of(&[0.0f32, 0.0, 0.0, 0.0]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let [nx, ny, nz] = config.volume_dims;
        let volume = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cloud volume"),
            size: wgpu::Extent3d {
                width: nx,
                height: ny,
                depth_or_array_layers: nz,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let volume_view = volume.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D3),
            ..Default::default()
        });
        let volume_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("cloud volume sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let (detail_texture, detail_view) = create_detail_noise(device, queue);
        let detail_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("cloud detail noise sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let shadow_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cloud shadow map"),
            size: wgpu::Extent3d {
                width: SHADOW_MAP_SIZE,
                height: SHADOW_MAP_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let shadow_view = shadow_texture.create_view(&Default::default());
        // Unshadowed default so a render before the first shadow pass is defined.
        let shadow_ones = vec![1.0f32; (SHADOW_MAP_SIZE * SHADOW_MAP_SIZE) as usize];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &shadow_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&shadow_ones),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SHADOW_MAP_SIZE * 4),
                rows_per_image: Some(SHADOW_MAP_SIZE),
            },
            wgpu::Extent3d {
                width: SHADOW_MAP_SIZE,
                height: SHADOW_MAP_SIZE,
                depth_or_array_layers: 1,
            },
        );
        let (output, output_view) = create_output(device, [1, 1]);
        let (cloud_buffer, cloud_view) = create_output(device, [1, 1]);
        let (resolved, resolved_view) = create_output(device, [1, 1]);
        let (history, history_view) = create_output(device, [1, 1]);
        let (environment_default, environment_view) = create_flat_rgba32(device);
        let (sun_default, sun_view) = create_flat_rgba32(device);
        let (background_default, background_view) = create_flat_rgba32(device);
        write_default(queue, &environment_default, [0.0, 0.0, 0.0, 1.0]);
        write_default(queue, &sun_default, [1.0, 1.0, 1.0, 1.0]);
        write_default(queue, &background_default, [0.0, 0.0, 0.0, 1.0]);

        let main_group = main_bind_group(
            device,
            &layout0,
            &params,
            &volume_view,
            &volume_sampler,
            &cloud_view,
            &atmosphere,
            &detail_view,
            &detail_sampler,
        );
        let environment_group =
            environment_bind_group(device, &layout1, &environment_view, &sun_view);
        let resolve_group = resolve_bind_group(
            device,
            &resolve_layout,
            &cloud_view,
            &history_view,
            &resolved_view,
            &temporal,
        );
        let shadow_group = shadow_bind_group(
            device,
            &shadow_layout,
            &volume_view,
            &volume_sampler,
            &shadow_params,
            &shadow_view,
        );
        let composite_group = composite_bind_group(
            device,
            &layout2,
            &resolved_view,
            &background_view,
            &output_view,
            &atmosphere,
            &composite_params,
            &shadow_view,
        );

        // Clear the volume once so a render before upload is defined.
        let zeros = vec![f16::from_f32(0.0).to_bits(); (nx * ny * nz * 4) as usize];
        write_volume(queue, &volume, [nx, ny, nz], bytemuck::cast_slice(&zeros));

        Ok(Self {
            config,
            pipeline,
            composite_pipeline,
            resolve_pipeline,
            shadow_pipeline,
            layout0,
            layout1,
            layout2,
            resolve_layout,
            shadow_layout,
            params,
            temporal,
            shadow_params,
            composite_params,
            shadow_texture,
            shadow_view,
            shadow_group,
            shadow_valid: false,
            shadow_dirty: true,
            shadow_sun: [0.0; 3],
            atmosphere,
            overcast: 0.0,
            storm_severity: 0.0,
            volume,
            volume_view,
            volume_sampler,
            detail_texture,
            detail_view,
            detail_sampler,
            detail_drift: [0.0; 2],
            detail_drift_delta: 0.0,
            volume_drift: [0.0; 2],
            cloud_buffer,
            cloud_view,
            resolved,
            resolved_view,
            history,
            history_view,
            output,
            output_view,
            main_group,
            environment_group,
            composite_group,
            resolve_group,
            environment_default,
            sun_default,
            background_default,
            solar_weights: [[0.0; 4]; 3],
            size: [1, 1],
            stats: Stats::default(),
            volume_descriptor: None,
            volume_uploaded: false,
            prev_basis: ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
            prev_tan_half: 0.0,
            prev_origin: [0.0; 3],
            history_valid: false,
            last_sun_dir: [0.0; 3],
            last_overcast: 0.0,
            last_storm_severity: 0.0,
        })
    }

    pub fn resize(&mut self, device: &wgpu::Device, size: [u32; 2]) -> bool {
        let size = [size[0].max(1), size[1].max(1)];
        if size == self.size && self.output.width() == size[0] && self.output.height() == size[1] {
            return false;
        }
        self.size = size;
        let trace = trace_size(size, self.config.trace_scale);
        let (output, output_view) = create_output(device, size);
        let (cloud_buffer, cloud_view) = create_output(device, trace);
        let (resolved, resolved_view) = create_output(device, trace);
        let (history, history_view) = create_output(device, trace);
        self.output = output;
        self.output_view = output_view;
        self.cloud_buffer = cloud_buffer;
        self.cloud_view = cloud_view;
        self.resolved = resolved;
        self.resolved_view = resolved_view;
        self.history = history;
        self.history_view = history_view;
        self.history_valid = false;
        self.main_group = main_bind_group(
            device,
            &self.layout0,
            &self.params,
            &self.volume_view,
            &self.volume_sampler,
            &self.cloud_view,
            &self.atmosphere,
            &self.detail_view,
            &self.detail_sampler,
        );
        self.resolve_group = resolve_bind_group(
            device,
            &self.resolve_layout,
            &self.cloud_view,
            &self.history_view,
            &self.resolved_view,
            &self.temporal,
        );
        let background_view = self.background_default.create_view(&Default::default());
        self.composite_group = composite_bind_group(
            device,
            &self.layout2,
            &self.resolved_view,
            &background_view,
            &self.output_view,
            &self.atmosphere,
            &self.composite_params,
            &self.shadow_view,
        );
        self.stats.last_output = size;
        self.stats.last_trace = trace;
        true
    }

    pub fn upload_volume(
        &mut self,
        queue: &wgpu::Queue,
        data: &[f32],
        descriptor: VolumeDescriptor,
    ) -> Result<()> {
        // Keep the original f32 validation contract for this public entry point.
        // The encoded payload is validated again by `upload_volume_half`.
        validate_volume(&self.config, data, descriptor)?;
        let half = crate::encode_volume_half(data);
        self.upload_volume_half(queue, &half, descriptor)
    }

    /// Upload a cloud volume that has already been encoded as half-precision
    /// bits (see [`crate::encode_volume_half`]).
    ///
    /// Validation and the GPU write are the same as [`Self::upload_volume`]; only
    /// the f32 -> f16 conversion is skipped, so a caller that reconstructed and
    /// encoded the volume on a worker thread can hand the finished payload to the
    /// render thread without doing the conversion there.
    pub fn upload_volume_half(
        &mut self,
        queue: &wgpu::Queue,
        data: &[u16],
        descriptor: VolumeDescriptor,
    ) -> Result<()> {
        validate_volume_half(&self.config, data, descriptor)?;
        let [nx, ny, nz] = descriptor.dims;
        write_volume(
            queue,
            &self.volume,
            [nx, ny, nz],
            bytemuck::cast_slice(data),
        );
        self.volume_descriptor = Some(descriptor);
        self.volume_uploaded = true;
        self.shadow_dirty = true;
        self.stats.volume_uploads += 1;
        Ok(())
    }

    /// Whether a volume has been uploaded since this renderer was created.
    ///
    /// The renderer always owns a valid (all-zero) volume texture, so
    /// [`Self::render`] is defined before the first upload; it just renders an
    /// empty cloud field. Applications that rebuild volumes asynchronously can
    /// use this to keep the first presented frame correct.
    pub fn has_volume(&self) -> bool {
        self.volume_uploaded
    }

    pub fn set_environment(
        &mut self,
        device: &wgpu::Device,
        environment: &wgpu::TextureView,
        sun_transmittance: &wgpu::TextureView,
        solar_weights: [[f32; 4]; 3],
    ) {
        self.solar_weights = solar_weights;
        self.environment_group =
            environment_bind_group(device, &self.layout1, environment, sun_transmittance);
    }

    pub fn set_background(&mut self, device: &wgpu::Device, background: &wgpu::TextureView) {
        self.composite_group = composite_bind_group(
            device,
            &self.layout2,
            &self.resolved_view,
            background,
            &self.output_view,
            &self.atmosphere,
            &self.composite_params,
            &self.shadow_view,
        );
    }

    /// Set the weather-driven overcast occlusion in [0, 1] (NaN becomes 0).
    /// At 0 the composite is the plain cloud + transmittance * background; at
    /// 1 the sky/ground background and cloud radiance are strongly dimmed,
    /// approximating the diffuse illumination under an overcast deck. This is
    /// an application-level coupling, not a transported cloud-shadow field.
    pub fn set_overcast(&mut self, overcast: f32) {
        self.overcast = clamp_overcast(overcast);
    }

    /// Drop the temporal history, e.g. after the application replaced the
    /// weather state behind the cloud volume. The next resolved frame uses the
    /// current trace with no history blending.
    pub fn reset_history(&mut self) {
        self.history_valid = false;
    }

    /// Drift of the sub-grid detail noise in world space (metres). The
    /// application advances it with the same wind advection as the weather
    /// volume so the high-frequency structure moves with the cloud rather
    /// than swimming through it. Non-finite values become zero.
    pub fn set_detail_drift(&mut self, drift_m: [f32; 2]) {
        let drift = clamp_detail_drift(drift_m);
        let dx = drift[0] - self.detail_drift[0];
        let dy = drift[1] - self.detail_drift[1];
        self.detail_drift_delta = (dx * dx + dy * dy).sqrt();
        self.detail_drift = drift;
    }

    /// Visual base-volume drift in world space (metres, x/z).
    ///
    /// The weather volume is only rebuilt every few hundred milliseconds, but
    /// the cloud field should keep moving with the wind between uploads.
    /// This offset is added to the 3D texture sampling position, so it slides
    /// the reconstructed volume continuously at frame rate instead of letting
    /// the motion step at the upload cadence. It is a rendering interpolation
    /// (the volume data is not resampled); the application resets it to zero
    /// when a new volume snapshot is uploaded. Non-finite values become zero.
    pub fn set_volume_drift(&mut self, drift_m: [f32; 2]) {
        self.volume_drift = clamp_detail_drift(drift_m);
    }

    /// Set the storm severity in [0, 1.5] (lightning / hail / very heavy rain /
    /// deep water-loaded towers). Only convective storms receive the extra
    /// near-dark composite drop; plain overcast keeps the ordinary overcast
    /// curve. Values above 1 feed the extra supercell tier in the composite.
    pub fn set_storm_severity(&mut self, severity: f32) {
        self.storm_severity = clamp_storm_severity(severity);
    }

    pub fn render(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        camera: Camera,
        sun: Sun,
    ) -> Result<()> {
        let (forward, right, up) = camera_basis(camera.yaw_deg, camera.pitch_deg);
        let sun_length = (sun.direction[0] * sun.direction[0]
            + sun.direction[1] * sun.direction[1]
            + sun.direction[2] * sun.direction[2])
            .sqrt();
        if !sun_length.is_finite() || sun_length <= 1.0e-6 {
            return Err("sun direction is zero or non-finite".into());
        }
        if !camera.origin_m.iter().all(|v| v.is_finite())
            || !camera.fov_y_deg.is_finite()
            || !(1.0..170.0).contains(&camera.fov_y_deg)
        {
            return Err("camera contains invalid values".into());
        }
        let sun_dir = [
            sun.direction[0] / sun_length,
            sun.direction[1] / sun_length,
            sun.direction[2] / sun_length,
        ];
        let descriptor = self
            .volume_descriptor
            .ok_or("render called before upload_volume")?;
        let [width, height] = self.size;
        let trace = trace_size(self.size, self.config.trace_scale);
        let tan_half = (f64::from(camera.fov_y_deg) * 0.5).to_radians().tan() as f32;
        // Invalidate temporal history on discontinuities the reprojection
        // cannot explain: camera translation, a large sun jump, or a step in
        // the application-supplied overcast/storm coefficients.
        let origin_delta = [0, 1, 2]
            .iter()
            .map(|&i| (camera.origin_m[i] as f32 - self.prev_origin[i]).powi(2))
            .sum::<f32>()
            .sqrt();
        let sun_dot = sun_dir[0] * self.last_sun_dir[0]
            + sun_dir[1] * self.last_sun_dir[1]
            + sun_dir[2] * self.last_sun_dir[2];
        if self.history_valid
            && (origin_delta > 0.5
                || sun_dot < 0.999_95
                || (tan_half - self.prev_tan_half).abs() > 1.0e-3
                || (self.overcast - self.last_overcast).abs() > 0.02
                || (self.storm_severity - self.last_storm_severity).abs() > 0.02)
        {
            self.history_valid = false;
        }
        let aspect = trace[0] as f32 / trace[1] as f32;
        // More history (small alpha) when the cloud moves little; fall back
        // toward the current frame under fast time-lapse advection.
        let alpha = temporal_alpha(self.detail_drift_delta);
        let temporal = TemporalParams {
            camera_forward: [forward[0], forward[1], forward[2], 0.0],
            camera_right: [right[0], right[1], right[2], 0.0],
            camera_up: [up[0], up[1], up[2], 0.0],
            prev_forward: [
                self.prev_basis.0[0],
                self.prev_basis.0[1],
                self.prev_basis.0[2],
                0.0,
            ],
            prev_right: [
                self.prev_basis.1[0],
                self.prev_basis.1[1],
                self.prev_basis.1[2],
                0.0,
            ],
            prev_up: [
                self.prev_basis.2[0],
                self.prev_basis.2[1],
                self.prev_basis.2[2],
                0.0,
            ],
            data: [
                tan_half,
                aspect,
                if self.history_valid { 1.0 } else { 0.0 },
                alpha,
            ],
        };
        queue.write_buffer(&self.temporal, 0, bytemuck::bytes_of(&temporal));
        let params = Params {
            camera_origin: [
                camera.origin_m[0],
                camera.origin_m[1],
                camera.origin_m[2],
                0.0,
            ],
            camera_forward: [forward[0], forward[1], forward[2], 0.0],
            camera_right: [right[0], right[1], right[2], 0.0],
            camera_up: [up[0], up[1], up[2], 0.0],
            volume_origin: [
                descriptor.origin_m[0],
                descriptor.origin_m[1],
                descriptor.origin_m[2],
                0.0,
            ],
            volume_spacing: [
                descriptor.spacing_m[0],
                descriptor.spacing_m[1],
                descriptor.spacing_m[2],
                0.0,
            ],
            volume_dims: [
                descriptor.dims[0],
                descriptor.dims[1],
                descriptor.dims[2],
                self.config.samples,
            ],
            sun_dir: [sun_dir[0], sun_dir[1], sun_dir[2], 0.0],
            solar_weights: self.solar_weights,
            viewport: [
                width as f32,
                height as f32,
                tan_half,
                self.stats.frames as f32,
            ],
            params: [
                self.config.max_steps as f32,
                self.config.extinction_scale,
                self.config.ambient_scale,
                self.config.ms_octaves as f32,
            ],
            detail: [
                self.config.detail_strength,
                self.detail_drift[0],
                self.detail_drift[1],
                self.config.sun_steps as f32,
            ],
            // z/w = visual base-volume drift (metres, x/z): the shader shifts
            // the volume sampling so the cloud keeps moving between rebuilds.
            aerial: [
                self.config.aerial_distance_m,
                1.0,
                self.volume_drift[0],
                self.volume_drift[1],
            ],
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        queue.write_buffer(
            &self.atmosphere,
            0,
            bytemuck::bytes_of(&atmosphere_bytes(self.overcast, self.storm_severity)),
        );
        let volume_extent = [
            descriptor.spacing_m[0] * descriptor.dims[0] as f32,
            descriptor.spacing_m[1] * descriptor.dims[1] as f32,
            descriptor.spacing_m[2] * descriptor.dims[2] as f32,
        ];
        let shadow_strength = self.config.cloud_shadow_strength;
        let composite = CompositeParams {
            camera_origin: [
                camera.origin_m[0],
                camera.origin_m[1],
                camera.origin_m[2],
                0.0,
            ],
            camera_forward: [forward[0], forward[1], forward[2], 0.0],
            camera_right: [right[0], right[1], right[2], 0.0],
            camera_up: [up[0], up[1], up[2], 0.0],
            volume_domain: [
                descriptor.origin_m[0],
                descriptor.origin_m[2],
                volume_extent[0],
                volume_extent[2],
            ],
            data: [
                tan_half,
                width as f32 / height.max(1) as f32,
                shadow_strength,
                0.0,
            ],
        };
        queue.write_buffer(&self.composite_params, 0, bytemuck::bytes_of(&composite));
        if shadow_strength > 0.0 {
            let sun_dot = sun_dir[0] * self.shadow_sun[0]
                + sun_dir[1] * self.shadow_sun[1]
                + sun_dir[2] * self.shadow_sun[2];
            let needs_shadow = !self.shadow_valid || self.shadow_dirty || sun_dot < 0.999_95;
            if needs_shadow {
                let shadow = ShadowParams {
                    sun: [sun_dir[0], sun_dir[1], sun_dir[2], 0.0],
                    volume_origin: [
                        descriptor.origin_m[0],
                        descriptor.origin_m[1],
                        descriptor.origin_m[2],
                        0.0,
                    ],
                    volume_spacing: [
                        descriptor.spacing_m[0],
                        descriptor.spacing_m[1],
                        descriptor.spacing_m[2],
                        0.0,
                    ],
                    volume_dims: [
                        descriptor.dims[0],
                        descriptor.dims[1],
                        descriptor.dims[2],
                        0,
                    ],
                    domain: [
                        descriptor.origin_m[0],
                        descriptor.origin_m[2],
                        volume_extent[0],
                        volume_extent[2],
                    ],
                };
                queue.write_buffer(&self.shadow_params, 0, bytemuck::bytes_of(&shadow));
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("cloud shadow pass"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.shadow_pipeline);
                pass.set_bind_group(0, &self.shadow_group, &[]);
                pass.dispatch_workgroups(
                    SHADOW_MAP_SIZE.div_ceil(8),
                    SHADOW_MAP_SIZE.div_ceil(8),
                    1,
                );
                self.shadow_valid = true;
                self.shadow_dirty = false;
                self.shadow_sun = sun_dir;
            }
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("cloud volume pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.main_group, &[]);
            pass.set_bind_group(1, &self.environment_group, &[]);
            pass.dispatch_workgroups(trace[0].div_ceil(8), trace[1].div_ceil(8), 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("cloud temporal resolve pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.resolve_pipeline);
            pass.set_bind_group(0, &self.resolve_group, &[]);
            pass.dispatch_workgroups(trace[0].div_ceil(8), trace[1].div_ceil(8), 1);
        }
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.resolved,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.history,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: trace[0],
                height: trace[1],
                depth_or_array_layers: 1,
            },
        );
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("cloud composite pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.composite_pipeline);
            pass.set_bind_group(0, &self.composite_group, &[]);
            pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
        }
        self.stats.frames += 1;
        self.stats.last_steps = self.config.max_steps;
        self.stats.last_output = [width, height];
        self.stats.last_trace = trace;
        self.prev_basis = (forward, right, up);
        self.prev_tan_half = tan_half;
        self.prev_origin = [camera.origin_m[0], camera.origin_m[1], camera.origin_m[2]];
        self.history_valid = true;
        self.last_sun_dir = sun_dir;
        self.last_overcast = self.overcast;
        self.last_storm_severity = self.storm_severity;
        self.detail_drift_delta = 0.0;
        Ok(())
    }

    pub fn target_view(&self) -> &wgpu::TextureView {
        &self.output_view
    }

    /// Output texture for readback/validation. Applications normally present
    /// the view; the texture accessor keeps the audit self-contained.
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

const SHADOW_MAP_SIZE: u32 = 512;
const DETAIL_DIM: u32 = 64;

/// Deterministic, tileable 3D noise for sub-voxel cloud detail.
///
/// The weather volume is only 96x64x96 over 32x14x32 km (voxels of 333 m x
/// 219 m), so linear filtering alone produces vertical "curtains" instead of
/// cumulus billows. This texture supplies two octaves of detail at 8 km and
/// 2 km tile sizes; the cloud shader erodes the coarse density with it. It is
/// a visual sub-grid turbulence model: it does not add or remove water from
/// the weather model's liquid-water path.
fn create_detail_noise(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::Texture, wgpu::TextureView) {
    let n = DETAIL_DIM;
    let mut broad = vec![0.0f32; (n * n * n) as usize];
    let mut mid = vec![0.0f32; (n * n * n) as usize];
    let mut fine = vec![0.0f32; (n * n * n) as usize];
    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                let p = [
                    (x as f32 + 0.5) / n as f32,
                    (y as f32 + 0.5) / n as f32,
                    (z as f32 + 0.5) / n as f32,
                ];
                let index = (x + n * (y + n * z)) as usize;
                broad[index] = periodic_fbm(0x5EED_1234, p, 4, 3);
                mid[index] = periodic_fbm(0xC10D_5678, p, 16, 2);
                fine[index] = periodic_fbm(0xA11C_E5EE, p, 32, 1);
            }
        }
    }
    // FBM of value noise has a narrow distribution around 0.5; without a
    // contrast stretch the erosion factor barely leaves 1.0. Normalise each
    // channel to its own range so the detail actually carves density.
    normalise_channel(&mut broad);
    normalise_channel(&mut mid);
    normalise_channel(&mut fine);
    let mut data = vec![0u8; (n * n * n * 4) as usize];
    for index in 0..(n * n * n) as usize {
        let index4 = index * 4;
        data[index4] = to_u8(broad[index]);
        data[index4 + 1] = to_u8(mid[index]);
        data[index4 + 2] = to_u8(fine[index]);
        data[index4 + 3] = 255;
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cloud detail noise"),
        size: wgpu::Extent3d {
            width: n,
            height: n,
            depth_or_array_layers: n,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(n * 4),
            rows_per_image: Some(n),
        },
        wgpu::Extent3d {
            width: n,
            height: n,
            depth_or_array_layers: n,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D3),
        ..Default::default()
    });
    (texture, view)
}

fn to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Map a channel to its full 0..1 range with a soft S-curve so the detail
/// erosion has contrast instead of hovering around the FBM mean.
fn normalise_channel(values: &mut [f32]) {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for &value in values.iter() {
        min = min.min(value);
        max = max.max(value);
    }
    let span = (max - min).max(1.0e-6);
    for value in values.iter_mut() {
        let t = (*value - min) / span;
        *value = t * t * (3.0 - 2.0 * t);
    }
}

fn detail_hash(seed: u32, x: i32, y: i32, z: i32) -> f32 {
    let mut h = seed
        ^ (x as u32).wrapping_mul(0x9E37_79B1)
        ^ (y as u32).wrapping_mul(0x85EB_CA77)
        ^ (z as u32).wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x2974_5A2D);
    h ^= h >> 15;
    (h >> 8) as f32 * (1.0 / 16_777_216.0)
}

fn periodic_value_noise(seed: u32, point: [f32; 3], frequency: u32) -> f32 {
    let frequency = frequency.max(1);
    let f = frequency as f32;
    let p = [point[0] * f, point[1] * f, point[2] * f];
    let base = [p[0].floor(), p[1].floor(), p[2].floor()];
    let mut t = [p[0] - base[0], p[1] - base[1], p[2] - base[2]];
    for value in &mut t {
        *value = *value * *value * (3.0 - 2.0 * *value);
    }
    let period = frequency as i32;
    let wrap = |value: i32| value.rem_euclid(period);
    let x0 = wrap(base[0] as i32);
    let y0 = wrap(base[1] as i32);
    let z0 = wrap(base[2] as i32);
    let x1 = wrap(base[0] as i32 + 1);
    let y1 = wrap(base[1] as i32 + 1);
    let z1 = wrap(base[2] as i32 + 1);
    let c000 = detail_hash(seed, x0, y0, z0);
    let c100 = detail_hash(seed, x1, y0, z0);
    let c010 = detail_hash(seed, x0, y1, z0);
    let c110 = detail_hash(seed, x1, y1, z0);
    let c001 = detail_hash(seed, x0, y0, z1);
    let c101 = detail_hash(seed, x1, y0, z1);
    let c011 = detail_hash(seed, x0, y1, z1);
    let c111 = detail_hash(seed, x1, y1, z1);
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let x00 = lerp(c000, c100, t[0]);
    let x10 = lerp(c010, c110, t[0]);
    let x01 = lerp(c001, c101, t[0]);
    let x11 = lerp(c011, c111, t[0]);
    let y0 = lerp(x00, x10, t[1]);
    let y1 = lerp(x01, x11, t[1]);
    lerp(y0, y1, t[2])
}

fn periodic_fbm(seed: u32, point: [f32; 3], base_frequency: u32, octaves: u32) -> f32 {
    let mut sum = 0.0;
    let mut weight = 0.0;
    let mut amplitude = 1.0;
    let mut frequency = base_frequency.max(1);
    for _ in 0..octaves.max(1) {
        sum += amplitude * periodic_value_noise(seed, point, frequency);
        weight += amplitude;
        amplitude *= 0.5;
        frequency = (frequency * 2).min(DETAIL_DIM);
    }
    sum / weight.max(1.0e-6)
}

/// Trace buffer size: output scaled by `trace_scale` on each axis (clamped to
/// at least one pixel; odd sizes round up so the last row/column is covered).
fn trace_size(size: [u32; 2], trace_scale: f32) -> [u32; 2] {
    let size = [size[0].max(1), size[1].max(1)];
    let scale = if trace_scale.is_finite() {
        trace_scale.clamp(0.25, 1.0)
    } else {
        1.0
    };
    if scale >= 1.0 {
        size
    } else {
        [
            ((size[0] as f32 * scale).round() as u32).max(1),
            ((size[1] as f32 * scale).round() as u32).max(1),
        ]
    }
}

fn create_output(device: &wgpu::Device, size: [u32; 2]) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cloud output"),
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
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    (texture, view)
}

fn create_flat_rgba32(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cloud default texture"),
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

fn write_default(queue: &wgpu::Queue, texture: &wgpu::Texture, value: [f32; 4]) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&value),
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
}

fn main_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    params: &wgpu::Buffer,
    volume: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    output: &wgpu::TextureView,
    atmosphere: &wgpu::Buffer,
    detail: &wgpu::TextureView,
    detail_sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cloud main bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(volume),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(output),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: atmosphere.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(detail),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::Sampler(detail_sampler),
            },
        ],
    })
}

fn environment_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    environment: &wgpu::TextureView,
    sun: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cloud environment bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(environment),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(sun),
            },
        ],
    })
}

fn resolve_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    raw: &wgpu::TextureView,
    history: &wgpu::TextureView,
    resolved: &wgpu::TextureView,
    temporal: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cloud resolve bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(raw),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(history),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(resolved),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: temporal.as_entire_binding(),
            },
        ],
    })
}

fn composite_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    cloud: &wgpu::TextureView,
    background: &wgpu::TextureView,
    output: &wgpu::TextureView,
    atmosphere: &wgpu::Buffer,
    composite_params: &wgpu::Buffer,
    shadow: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cloud composite bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(cloud),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(background),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(output),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: atmosphere.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: composite_params.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(shadow),
            },
        ],
    })
}

fn shadow_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    volume: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    params: &wgpu::Buffer,
    output: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cloud shadow bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(volume),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(output),
            },
        ],
    })
}

fn write_volume(queue: &wgpu::Queue, texture: &wgpu::Texture, dims: [u32; 3], data: &[u8]) {
    // wgpu requires 256-byte row alignment for texture copies. Volumes whose row
    // pitch is already a multiple of that (the 128-voxel-wide workbench volume
    // is 1024 B per row) can be copied straight from the caller's payload,
    // avoiding a full 12.6 MB repack on the render thread; otherwise pad rows.
    let row_bytes = dims[0] as usize * 8;
    let padded_row = row_bytes.div_ceil(256) * 256;
    let mut padded;
    let upload: &[u8] = if padded_row == row_bytes {
        data
    } else {
        padded = vec![0u8; padded_row * dims[1] as usize * dims[2] as usize];
        for z in 0..dims[2] as usize {
            for y in 0..dims[1] as usize {
                let src = (z * dims[1] as usize + y) * row_bytes;
                let dst = (z * dims[1] as usize + y) * padded_row;
                padded[dst..dst + row_bytes].copy_from_slice(&data[src..src + row_bytes]);
            }
        }
        &padded
    };
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        upload,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(padded_row as u32),
            rows_per_image: Some(dims[1]),
        },
        wgpu::Extent3d {
            width: dims[0],
            height: dims[1],
            depth_or_array_layers: dims[2],
        },
    );
}

fn validate_volume(config: &Config, data: &[f32], descriptor: VolumeDescriptor) -> Result<()> {
    validate_volume_layout(config, data.len(), descriptor)?;
    if data.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return Err("cloud volume contains invalid geometry or values".into());
    }
    Ok(())
}

/// Validate a half-encoded payload with the same contract as [`validate_volume`]:
/// shapes must match the configured volume and every decoded value must be
/// finite and non-negative. The checks are done on the bit pattern instead of
/// decoding each value (6.3 M voxels would cost ~10 ms on the render thread);
/// a sign bit other than negative zero is the `f32 < 0.0` test and an all-ones
/// exponent is the `!is_finite` test.
fn validate_volume_half(config: &Config, data: &[u16], descriptor: VolumeDescriptor) -> Result<()> {
    validate_volume_layout(config, data.len(), descriptor)?;
    // Branchless accumulation so this 6.3 M-value scan stays wide; an early-exit
    // `any` loop here costs several ms on the render thread.
    let mut invalid = 0u16;
    for &bits in data {
        invalid |= u16::from(bits > NEGATIVE_ZERO);
        invalid |= u16::from((bits & EXPONENT_MASK) == EXPONENT_ALL_ONES);
    }
    if invalid != 0 {
        return Err("cloud volume contains invalid geometry or values".into());
    }
    Ok(())
}

const NEGATIVE_ZERO: u16 = 0x8000;
const EXPONENT_MASK: u16 = 0x7C00;
const EXPONENT_ALL_ONES: u16 = 0x7C00;

fn validate_volume_layout(
    config: &Config,
    data_len: usize,
    descriptor: VolumeDescriptor,
) -> Result<()> {
    if descriptor.dims != config.volume_dims {
        return Err(format!(
            "cloud volume dimensions {:?} do not match configured {:?}",
            descriptor.dims, config.volume_dims
        )
        .into());
    }
    let expected = descriptor
        .dims
        .iter()
        .map(|&v| v as usize)
        .product::<usize>()
        * 4;
    if data_len != expected {
        return Err(format!("cloud volume has {data_len} values, expected {expected}").into());
    }
    if !descriptor
        .origin_m
        .iter()
        .chain(descriptor.spacing_m.iter())
        .all(|v| v.is_finite())
        || descriptor.spacing_m.iter().any(|&v| v <= 0.0)
    {
        return Err("cloud volume contains invalid geometry or values".into());
    }
    Ok(())
}

/// Camera basis used by the shader, exposed for CPU tests.
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

/// Optical depth of a homogeneous slab, used for CPU sanity checks.
pub fn slab_optical_depth(lwc: f64, reff_m: f64, thickness_m: f64) -> f64 {
    optics::water_extinction(lwc, reff_m) * thickness_m
}

/// Uniform payload shared by the volume and composite passes:
/// `[overcast occlusion, storm severity, 0, 0]`.
fn atmosphere_bytes(overcast: f32, storm_severity: f32) -> [f32; 4] {
    [
        clamp_overcast(overcast),
        clamp_storm_severity(storm_severity),
        0.0,
        0.0,
    ]
}

/// Clamp a convective storm severity; non-finite input becomes 0. Values above
/// 1.5 are clamped (the extra near-dark tier saturates there).
pub fn clamp_storm_severity(severity: f32) -> f32 {
    if severity.is_finite() {
        severity.clamp(0.0, 1.5)
    } else {
        0.0
    }
}

/// Temporal blend alpha (current-frame weight). History dominates at 0.12
/// under slow cloud motion and is progressively rejected as the per-frame
/// cloud drift grows, bounding time-lapse ghosting. Non-finite input is
/// treated as no motion.
pub fn temporal_alpha(drift_delta_m: f32) -> f32 {
    let drift = if drift_delta_m.is_finite() {
        drift_delta_m.max(0.0)
    } else {
        0.0
    };
    (0.08 + (drift / 200.0).clamp(0.0, 0.54)).min(0.62)
}

/// Clamp a sub-grid detail drift; non-finite components become zero.
pub fn clamp_detail_drift(drift_m: [f32; 2]) -> [f32; 2] {
    if drift_m.iter().all(|v| v.is_finite()) {
        drift_m
    } else {
        [0.0; 2]
    }
}

/// Clamp a weather-derived overcast occlusion; non-finite input becomes 0.
pub fn clamp_overcast(overcast: f32) -> f32 {
    if overcast.is_finite() {
        overcast.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Zero-density composite must return the background exactly.
pub fn composite(cloud: [f32; 3], transmittance: f32, background: [f32; 3]) -> [f32; 3] {
    composite_with_overcast(cloud, transmittance, background, 0.0, 0.0)
}

/// CPU mirror of the composite shader including the overcast dimming coupling.
pub fn composite_with_overcast(
    cloud: [f32; 3],
    transmittance: f32,
    background: [f32; 3],
    overcast: f32,
    storm_severity: f32,
) -> [f32; 3] {
    let overcast = clamp_overcast(overcast);
    // Mirror composite.wgsl: convective storm severity applies an extra
    // darkening step; severity above 1 (supercell-like towers) has an extra
    // near-dark tier. Skylight also trends neutral grey.
    let severity = clamp_storm_severity(storm_severity);
    let base_severity = severity.min(1.0);
    let extra = (severity - 1.0).clamp(0.0, 0.5);
    let background_scale = (1.0 + (0.26 - 1.0) * overcast)
        * (1.0 + (0.30 - 1.0) * base_severity)
        * (1.0 + (0.50 - 1.0) * extra * 2.0);
    let cloud_scale = (1.0 + (0.42 - 1.0) * overcast)
        * (1.0 + (0.34 - 1.0) * base_severity)
        * (1.0 + (0.40 - 1.0) * extra * 2.0);
    let luma = 0.2126 * background[0] + 0.7152 * background[1] + 0.0722 * background[2];
    let tint = 0.6 * overcast;
    let mut result = [0.0f32; 3];
    for channel in 0..3 {
        let tinted = background[channel] + (luma - background[channel]) * tint;
        result[channel] = cloud[channel] * cloud_scale + transmittance * tinted * background_scale;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_parses_validates_and_emits_spirv() {
        for source in [SHADER, COMPOSITE_SHADER, RESOLVE_SHADER, SHADOW_SHADER] {
            let module = naga::front::wgsl::parse_str(source)
                .unwrap_or_else(|e| panic!("{}", e.emit_to_string(source)));
            let info = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap();
            naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
                .unwrap();
        }
    }

    #[test]
    fn camera_basis_is_orthonormal() {
        for (yaw, pitch) in [
            (0.0f32, 0.0f32),
            (37.0, 15.0),
            (180.0, -30.0),
            (300.0, 60.0),
        ] {
            let (f, r, u) = camera_basis(yaw, pitch);
            let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
            assert!((dot(f, f) - 1.0).abs() < 1.0e-5);
            assert!((dot(r, r) - 1.0).abs() < 1.0e-5);
            assert!((dot(u, u) - 1.0).abs() < 1.0e-5);
            assert!(dot(f, r).abs() < 1.0e-5);
            assert!(dot(f, u).abs() < 1.0e-5);
            assert!(dot(r, u).abs() < 1.0e-5);
        }
    }

    #[test]
    fn zero_density_composite_is_exact_background() {
        let background = [0.25, 0.5, 1.0];
        assert_eq!(composite([0.0; 3], 1.0, background), background);
    }

    #[test]
    fn overcast_and_storm_severity_dim_background_and_cloud() {
        let background = [0.8, 0.9, 1.0];
        let luma = 0.2126 * background[0] + 0.7152 * background[1] + 0.0722 * background[2];
        let clear = composite_with_overcast([0.0; 3], 1.0, background, 0.0, 0.0);
        assert_eq!(clear, background);

        // Overcast alone (no convective storm) uses the plain overcast curve.
        let overcast = composite_with_overcast([0.0; 3], 1.0, background, 1.0, 0.0);
        for channel in 0..3 {
            let tinted = background[channel] + (luma - background[channel]) * 0.6;
            assert!((overcast[channel] - tinted * 0.26).abs() < 1.0e-6);
        }

        // Storm severity adds the extra near-dark drop: 0.26 * 0.30.
        let storm = composite_with_overcast([0.0; 3], 1.0, background, 1.0, 1.0);
        for channel in 0..3 {
            let tinted = background[channel] + (luma - background[channel]) * 0.6;
            assert!((storm[channel] - tinted * 0.26 * 0.30).abs() < 1.0e-6);
        }
        assert!(storm[0] < overcast[0]);

        let cloud_clear = composite_with_overcast([0.5; 3], 0.0, background, 0.0, 0.0);
        let cloud_overcast = composite_with_overcast([0.5; 3], 0.0, background, 1.0, 0.0);
        let cloud_storm = composite_with_overcast([0.5; 3], 0.0, background, 1.0, 1.0);
        assert!((cloud_overcast[0] - cloud_clear[0] * 0.42).abs() < 1.0e-6);
        assert!((cloud_storm[0] - cloud_clear[0] * 0.42 * 0.34).abs() < 1.0e-6);

        // Supercell-like towers (severity 1.5) take the extra near-dark tier:
        // background *0.26*0.30*0.50, cloud *0.42*0.34*0.40.
        let tower_bg = composite_with_overcast([0.0; 3], 1.0, background, 1.0, 1.5);
        for channel in 0..3 {
            let tinted = background[channel] + (luma - background[channel]) * 0.6;
            assert!((tower_bg[channel] - tinted * 0.26 * 0.30 * 0.50).abs() < 1.0e-6);
        }
        let tower_cloud = composite_with_overcast([0.5; 3], 0.0, background, 1.0, 1.5);
        assert!((tower_cloud[0] - cloud_clear[0] * 0.42 * 0.34 * 0.40).abs() < 1.0e-6);
        assert!(tower_cloud[0] < cloud_storm[0]);

        // Non-finite occlusion falls back to the clear composite.
        assert_eq!(
            composite_with_overcast([0.0; 3], 1.0, background, f32::NAN, 0.0),
            background
        );
    }

    #[test]
    fn atmosphere_uniform_keeps_the_supercell_severity_tier() {
        // Regression: the setter used to clamp with clamp_overcast, which made
        // the >1 supercell tier dead on the real GPU path.
        let bytes = atmosphere_bytes(0.93, 1.5);
        assert_eq!(bytes[0], 0.93);
        assert_eq!(bytes[1], 1.5);
        assert_eq!(atmosphere_bytes(0.5, f32::NAN)[1], 0.0);
        assert_eq!(atmosphere_bytes(2.0, -1.0), [1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn storm_severity_is_clamped() {
        assert_eq!(clamp_storm_severity(-1.0), 0.0);
        assert_eq!(clamp_storm_severity(1.0), 1.0);
        assert_eq!(clamp_storm_severity(1.25), 1.25);
        assert_eq!(clamp_storm_severity(2.0), 1.5);
        assert_eq!(clamp_storm_severity(f32::NAN), 0.0);
    }

    #[test]
    fn overcast_input_is_clamped() {
        assert_eq!(clamp_overcast(-1.0), 0.0);
        assert_eq!(clamp_overcast(0.0), 0.0);
        assert_eq!(clamp_overcast(0.5), 0.5);
        assert_eq!(clamp_overcast(1.0), 1.0);
        assert_eq!(clamp_overcast(2.0), 1.0);
        assert_eq!(clamp_overcast(f32::INFINITY), 0.0);
        assert_eq!(clamp_overcast(f32::NAN), 0.0);
    }

    #[test]
    fn temporal_alpha_rises_with_cloud_motion() {
        assert!((temporal_alpha(0.0) - 0.08).abs() < 1.0e-6);
        assert!(temporal_alpha(20.0) > temporal_alpha(0.0));
        assert!(temporal_alpha(10_000.0) <= 0.62);
        assert!((temporal_alpha(f32::NAN) - 0.08).abs() < 1.0e-6);
        assert!((temporal_alpha(-5.0) - 0.08).abs() < 1.0e-6);
    }

    #[test]
    fn trace_size_scales_without_losing_pixels() {
        assert_eq!(trace_size([1601, 901], 1.0), [1601, 901]);
        assert_eq!(trace_size([1601, 901], 0.5), [801, 451]);
        assert_eq!(trace_size([1600, 900], 0.55), [880, 495]);
        assert_eq!(trace_size([1600, 900], 0.8), [1280, 720]);
        assert_eq!(trace_size([0, 0], 0.5), [1, 1]);
        assert_eq!(trace_size([1600, 900], f32::NAN), [1600, 900]);
    }

    #[test]
    fn aerial_distance_is_validated() {
        let mut config = Config::default();
        config.aerial_distance_m = -1.0;
        assert!(config.validate().is_err());
        config.aerial_distance_m = f32::NAN;
        assert!(config.validate().is_err());
        config.aerial_distance_m = 0.0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn cloud_shadow_strength_is_validated() {
        let mut config = Config::default();
        config.cloud_shadow_strength = -0.1;
        assert!(config.validate().is_err());
        config.cloud_shadow_strength = 1.1;
        assert!(config.validate().is_err());
        config.cloud_shadow_strength = 0.9;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn detail_drift_sanitises_non_finite_values() {
        assert_eq!(clamp_detail_drift([12.0, -3.0]), [12.0, -3.0]);
        assert_eq!(clamp_detail_drift([f32::NAN, 1.0]), [0.0, 0.0]);
        assert_eq!(clamp_detail_drift([1.0, f32::INFINITY]), [0.0, 0.0]);
    }

    #[test]
    fn normalised_detail_channels_span_unit_range() {
        let mut values = vec![0.45, 0.5, 0.55, 0.52, 0.48];
        normalise_channel(&mut values);
        let min = values.iter().copied().fold(f32::INFINITY, f32::min);
        let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(min.abs() < 1.0e-6, "{min}");
        assert!((max - 1.0).abs() < 1.0e-6, "{max}");
    }

    #[test]
    fn upload_validation_rejects_wrong_shape_and_negative_values() {
        let config = Config {
            volume_dims: [4, 4, 4],
            ..Config::default()
        };
        let descriptor = VolumeDescriptor {
            dims: [4, 4, 4],
            origin_m: [0.0; 3],
            spacing_m: [1.0; 3],
        };
        assert!(validate_volume(&config, &vec![1.0; 4 * 4 * 4 * 4], descriptor).is_ok());
        assert!(validate_volume(&config, &vec![1.0; 3], descriptor).is_err());
        let mut bad = vec![1.0; 4 * 4 * 4 * 4];
        bad[0] = -1.0;
        assert!(validate_volume(&config, &bad, descriptor).is_err());
    }

    /// The async weather pass replaces the renderer's inline f32->f16 loop with
    /// `encode_volume_half`; the encoded payload must be bit-identical.
    #[test]
    fn half_encoding_matches_the_renderer_conversion() {
        let values = [0.0f32, 1.0, 0.5, 1.0e-8, -0.0, 12.5, 1.0e-20, 65_504.0];
        let encoded = crate::encode_volume_half(&values);
        let expected: Vec<u16> = values
            .iter()
            .map(|&value| f16::from_f32(value).to_bits())
            .collect();
        assert_eq!(encoded, expected);
    }

    #[test]
    fn half_volume_validation_matches_the_float_contract() {
        let config = Config {
            volume_dims: [4, 4, 4],
            ..Config::default()
        };
        let descriptor = VolumeDescriptor {
            dims: [4, 4, 4],
            origin_m: [0.0; 3],
            spacing_m: [1.0; 3],
        };
        let values: Vec<f32> = (0..4 * 4 * 4 * 4)
            .map(|index| index as f32 * 1.0e-6)
            .collect();
        assert!(validate_volume(&config, &values, descriptor).is_ok());
        let half = crate::encode_volume_half(&values);
        assert!(validate_volume_half(&config, &half, descriptor).is_ok());
        assert!(validate_volume_half(&config, &half[..half.len() - 1], descriptor).is_err());

        for invalid in [
            f16::from_f32(-1.0).to_bits(),
            f16::from_f32(f32::NAN).to_bits(),
            f16::from_f32(f32::INFINITY).to_bits(),
            // Negative subnormal, not just negative zero.
            0x8001,
        ] {
            let mut bad = half.clone();
            bad[0] = invalid;
            assert!(validate_volume_half(&config, &bad, descriptor).is_err());
        }

        // Negative zero is still zero: the f32 path accepts it, so does the
        // half path (the bit-pattern check must not reject 0x8000).
        let mut negative_zero = half.clone();
        negative_zero[0] = f16::from_f32(-0.0).to_bits();
        assert!(validate_volume_half(&config, &negative_zero, descriptor).is_ok());
    }

    #[test]
    fn slab_optical_depth_matches_relation() {
        let tau = slab_optical_depth(0.3e-3, 10.0e-6, 500.0);
        assert!((tau - 22.5).abs() < 1.0e-9, "{tau}");
    }
}
