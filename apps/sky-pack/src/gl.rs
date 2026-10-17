//! Minimal OpenGL compatibility-profile context plus the handful of helpers the
//! offline shader-pack validation harness needs.
//!
//! Minecraft is not installed in this repository's development environment, so
//! the shipped `#version 330 compatibility` shader sources are compiled and
//! executed with the real OpenGL driver through a hidden Win32 window. This
//! exercises the same GLSL a shader pack is written in (including `ftransform`,
//! `gl_MultiTexCoord0`, `texelFetch`, RGBA16F/32F targets) without the game.

use std::ffi::{CString, c_void};
use std::ptr;

use glow::HasContext;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::event_loop::EventLoop;
use winit::window::WindowAttributes;

type Hwnd = isize;
type Hdc = isize;
type Hglrc = isize;

#[repr(C)]
struct PixelFormatDescriptor {
    n_size: u16,
    n_version: u16,
    dw_flags: u32,
    i_pixel_type: u8,
    c_color_bits: u8,
    c_red_bits: u8,
    c_red_shift: u8,
    c_green_bits: u8,
    c_green_shift: u8,
    c_blue_bits: u8,
    c_blue_shift: u8,
    c_alpha_bits: u8,
    c_alpha_shift: u8,
    c_accum_bits: u8,
    c_accum_red_bits: u8,
    c_accum_green_bits: u8,
    c_accum_blue_bits: u8,
    c_accum_alpha_bits: u8,
    c_depth_bits: u8,
    c_stencil_bits: u8,
    c_aux_buffers: u8,
    i_layer_type: u8,
    b_reserved: u8,
    dw_layer_mask: u32,
    dw_visible_mask: u32,
    dw_damage_mask: u32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetDC(hwnd: Hwnd) -> Hdc;
    fn ReleaseDC(hwnd: Hwnd, hdc: Hdc) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn ChoosePixelFormat(hdc: Hdc, format: *const PixelFormatDescriptor) -> i32;
    fn SetPixelFormat(hdc: Hdc, format: i32, pfd: *const PixelFormatDescriptor) -> i32;
}

#[link(name = "opengl32")]
unsafe extern "system" {
    fn wglCreateContext(hdc: Hdc) -> Hglrc;
    fn wglMakeCurrent(hdc: Hdc, ctx: Hglrc) -> i32;
    fn wglDeleteContext(ctx: Hglrc) -> i32;
    fn wglGetProcAddress(name: *const u8) -> *const c_void;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryA(name: *const u8) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *const c_void;
}

fn cstr(bytes: &[u8]) -> CString {
    CString::new(bytes).expect("static C string")
}

/// Hidden-window WGL compatibility context. Rendering goes to framebuffers, so
/// the window is never shown.
pub struct GlContext {
    pub gl: glow::Context,
    hdc: Hdc,
    hwnd: Hwnd,
    glrc: Hglrc,
}

impl GlContext {
    #[allow(deprecated)]
    pub fn new() -> Result<Self, String> {
        let event_loop = EventLoop::new().map_err(|e| format!("event loop: {e}"))?;
        let window = event_loop
            .create_window(
                WindowAttributes::default()
                    .with_visible(false)
                    .with_title("sky-pack offline validation"),
            )
            .map_err(|e| format!("window: {e}"))?;
        let handle = window
            .window_handle()
            .map_err(|e| format!("window handle: {e}"))?;
        let hwnd = match handle.as_raw() {
            RawWindowHandle::Win32(h) => h.hwnd.get() as Hwnd,
            other => return Err(format!("unexpected raw window handle: {other:?}")),
        };
        std::mem::forget(window);
        std::mem::forget(event_loop);

        unsafe {
            let hdc = GetDC(hwnd);
            if hdc == 0 {
                return Err("GetDC failed".into());
            }
            let pfd = PixelFormatDescriptor {
                n_size: std::mem::size_of::<PixelFormatDescriptor>() as u16,
                n_version: 1,
                dw_flags: 0x0000_0004 | 0x0000_0020 | 0x0000_0001,
                i_pixel_type: 0,
                c_color_bits: 32,
                c_red_bits: 8,
                c_red_shift: 0,
                c_green_bits: 8,
                c_green_shift: 0,
                c_blue_bits: 8,
                c_blue_shift: 0,
                c_alpha_bits: 8,
                c_alpha_shift: 0,
                c_accum_bits: 0,
                c_accum_red_bits: 0,
                c_accum_green_bits: 0,
                c_accum_blue_bits: 0,
                c_accum_alpha_bits: 0,
                c_depth_bits: 24,
                c_stencil_bits: 8,
                c_aux_buffers: 0,
                i_layer_type: 0,
                b_reserved: 0,
                dw_layer_mask: 0,
                dw_visible_mask: 0,
                dw_damage_mask: 0,
            };
            let pixel_format = ChoosePixelFormat(hdc, &pfd);
            if pixel_format == 0 {
                return Err("ChoosePixelFormat failed".into());
            }
            if SetPixelFormat(hdc, pixel_format, &pfd) == 0 {
                return Err("SetPixelFormat failed".into());
            }
            let legacy = wglCreateContext(hdc);
            if legacy == 0 {
                return Err("wglCreateContext failed".into());
            }
            if wglMakeCurrent(hdc, legacy) == 0 {
                return Err("wglMakeCurrent(legacy) failed".into());
            }
            let create_name = cstr(b"wglCreateContextAttribsARB");
            let raw = wglGetProcAddress(create_name.as_ptr() as *const u8);
            if raw.is_null() {
                return Err("wglCreateContextAttribsARB unavailable".into());
            }
            let create: extern "system" fn(Hdc, Hglrc, *const i32) -> Hglrc =
                std::mem::transmute(raw);
            let attribs = [0x2091, 4, 0x2092, 6, 0x9126, 0x0000_0002, 0];
            let modern = create(hdc, 0, attribs.as_ptr());
            if modern == 0 {
                return Err("compatibility-profile context creation failed".into());
            }
            wglMakeCurrent(hdc, modern);
            wglDeleteContext(legacy);

            let opengl32 = LoadLibraryA(cstr(b"opengl32.dll").as_ptr() as *const u8);
            if opengl32.is_null() {
                return Err("LoadLibraryA(opengl32.dll) failed".into());
            }
            let loader = move |symbol: &str| -> *const c_void {
                let Ok(name) = CString::new(symbol) else {
                    return ptr::null();
                };
                let direct = GetProcAddress(opengl32, name.as_ptr() as *const u8);
                if !direct.is_null() {
                    return direct;
                }
                wglGetProcAddress(name.as_ptr() as *const u8)
            };
            let gl = glow::Context::from_loader_function(loader);
            Ok(Self {
                gl,
                hdc,
                hwnd,
                glrc: modern,
            })
        }
    }

    pub fn renderer_string(&self) -> String {
        unsafe {
            format!(
                "{} / {} ({})",
                self.gl.get_parameter_string(glow::VERSION),
                self.gl.get_parameter_string(glow::RENDERER),
                self.gl.get_parameter_string(glow::VENDOR)
            )
        }
    }
}

impl Drop for GlContext {
    fn drop(&mut self) {
        unsafe {
            wglMakeCurrent(0, 0);
            wglDeleteContext(self.glrc);
            ReleaseDC(self.hwnd, self.hdc);
        }
    }
}

// ---------------------------------------------------------------------------
// Textures and framebuffers
// ---------------------------------------------------------------------------
pub struct GlTexture {
    pub id: glow::Texture,
    pub width: u32,
    pub height: u32,
}

fn tex_parameters(gl: &glow::Context, nearest: bool) {
    unsafe {
        let filter = if nearest { glow::NEAREST } else { glow::LINEAR };
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, filter as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, filter as i32);
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
    }
}

pub fn texture_rgba32f(
    gl: &glow::Context,
    width: u32,
    height: u32,
    data: Option<&[f32]>,
) -> GlTexture {
    unsafe {
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        let id = gl.create_texture().expect("create texture");
        gl.bind_texture(glow::TEXTURE_2D, Some(id));
        tex_parameters(gl, false);
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA32F as i32,
            width as i32,
            height as i32,
            0,
            glow::RGBA,
            glow::FLOAT,
            glow::PixelUnpackData::Slice(data.map(bytemuck::cast_slice)),
        );
        gl.bind_texture(glow::TEXTURE_2D, None);
        GlTexture { id, width, height }
    }
}

pub fn texture_rgba16f(
    gl: &glow::Context,
    width: u32,
    height: u32,
    data: Option<&[u8]>,
) -> GlTexture {
    unsafe {
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        let id = gl.create_texture().expect("create texture");
        gl.bind_texture(glow::TEXTURE_2D, Some(id));
        tex_parameters(gl, false);
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA16F as i32,
            width as i32,
            height as i32,
            0,
            glow::RGBA,
            glow::HALF_FLOAT,
            glow::PixelUnpackData::Slice(data),
        );
        gl.bind_texture(glow::TEXTURE_2D, None);
        GlTexture { id, width, height }
    }
}

pub fn texture_rgba8(
    gl: &glow::Context,
    width: u32,
    height: u32,
    data: Option<&[u8]>,
) -> GlTexture {
    unsafe {
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        let id = gl.create_texture().expect("create texture");
        gl.bind_texture(glow::TEXTURE_2D, Some(id));
        tex_parameters(gl, true);
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            width as i32,
            height as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(data),
        );
        gl.bind_texture(glow::TEXTURE_2D, None);
        GlTexture { id, width, height }
    }
}

pub fn texture_r32f(gl: &glow::Context, width: u32, height: u32, data: &[f32]) -> GlTexture {
    unsafe {
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        let id = gl.create_texture().expect("create texture");
        gl.bind_texture(glow::TEXTURE_2D, Some(id));
        tex_parameters(gl, true);
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::R32F as i32,
            width as i32,
            height as i32,
            0,
            glow::RED,
            glow::FLOAT,
            glow::PixelUnpackData::Slice(Some(bytemuck::cast_slice(data))),
        );
        gl.bind_texture(glow::TEXTURE_2D, None);
        GlTexture { id, width, height }
    }
}

pub struct GlFramebuffer {
    pub fbo: glow::Framebuffer,
    pub color: GlTexture,
}

impl GlFramebuffer {
    pub fn new(
        gl: &glow::Context,
        width: u32,
        height: u32,
        format: u32,
        bytes_per_texel: u32,
    ) -> Result<Self, String> {
        let color = match format {
            glow::RGBA32F => texture_rgba32f(gl, width, height, None),
            glow::RGBA16F => texture_rgba16f(gl, width, height, None),
            glow::RGBA8 => texture_rgba8(gl, width, height, None),
            other => return Err(format!("unsupported framebuffer format {other:#x}")),
        };
        unsafe {
            let fbo = gl.create_framebuffer().map_err(|e| e.to_string())?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(color.id),
                0,
            );
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            if status != glow::FRAMEBUFFER_COMPLETE {
                return Err(format!(
                    "framebuffer incomplete ({status:#x}) {width}x{height} format {format:#x} {bytes_per_texel}"
                ));
            }
            Ok(Self { fbo, color })
        }
    }

    pub fn bind(&self, gl: &glow::Context) {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.viewport(0, 0, self.color.width as i32, self.color.height as i32);
        }
    }

    pub fn read_rgba_f32(&self, gl: &glow::Context) -> Vec<f32> {
        let mut pixels = vec![0.0f32; (self.color.width * self.color.height * 4) as usize];
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.read_pixels(
                0,
                0,
                self.color.width as i32,
                self.color.height as i32,
                glow::RGBA,
                glow::FLOAT,
                glow::PixelPackData::Slice(Some(bytemuck::cast_slice_mut(&mut pixels))),
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        pixels
    }

    pub fn read_rgba8(&self, gl: &glow::Context) -> Vec<u8> {
        let mut pixels = vec![0u8; (self.color.width * self.color.height * 4) as usize];
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.read_pixels(
                0,
                0,
                self.color.width as i32,
                self.color.height as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixels)),
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        pixels
    }
}

impl Drop for GlFramebuffer {
    fn drop(&mut self) {
        // The context owns the objects; leaking them is harmless for a one-shot
        // offline tool, so no glow calls are made from Drop.
    }
}

// ---------------------------------------------------------------------------
// Programs
// ---------------------------------------------------------------------------
pub const FULLSCREEN_VERTEX: &str = r#"#version 330 compatibility
in vec2 a_position;
out vec2 texcoord;
void main() {
    gl_Position = vec4(a_position, 0.0, 1.0);
    texcoord = a_position * 0.5 + 0.5;
}
"#;

pub struct GlProgram {
    pub id: glow::Program,
}

impl GlProgram {
    pub fn new(gl: &glow::Context, fragment_source: &str, label: &str) -> Result<Self, String> {
        unsafe {
            let vertex = gl
                .create_shader(glow::VERTEX_SHADER)
                .map_err(|e| e.to_string())?;
            gl.shader_source(vertex, FULLSCREEN_VERTEX);
            gl.compile_shader(vertex);
            if !gl.get_shader_compile_status(vertex) {
                return Err(format!(
                    "{label}: vertex compile failed: {}",
                    gl.get_shader_info_log(vertex)
                ));
            }
            compile_shader(gl, glow::FRAGMENT_SHADER, fragment_source, label)?;
            let fragment = gl
                .create_shader(glow::FRAGMENT_SHADER)
                .map_err(|e| e.to_string())?;
            gl.shader_source(fragment, fragment_source);
            gl.compile_shader(fragment);
            if !gl.get_shader_compile_status(fragment) {
                return Err(format!(
                    "{label}: fragment compile failed:\n{}",
                    gl.get_shader_info_log(fragment)
                ));
            }
            let program = gl.create_program().map_err(|e| e.to_string())?;
            gl.attach_shader(program, vertex);
            gl.attach_shader(program, fragment);
            gl.bind_attrib_location(program, 0, "a_position");
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                return Err(format!(
                    "{label}: link failed:\n{}",
                    gl.get_program_info_log(program)
                ));
            }
            gl.delete_shader(vertex);
            gl.delete_shader(fragment);
            Ok(Self { id: program })
        }
    }

    pub fn use_program(&self, gl: &glow::Context) {
        unsafe {
            gl.use_program(Some(self.id));
        }
    }

    pub fn uniform_i32(&self, gl: &glow::Context, name: &str, value: i32) {
        unsafe {
            if let Some(loc) = gl.get_uniform_location(self.id, name) {
                gl.uniform_1_i32(Some(&loc), value);
            }
        }
    }

    pub fn uniform_f32(&self, gl: &glow::Context, name: &str, value: f32) {
        unsafe {
            if let Some(loc) = gl.get_uniform_location(self.id, name) {
                gl.uniform_1_f32(Some(&loc), value);
            }
        }
    }

    pub fn uniform_vec3(&self, gl: &glow::Context, name: &str, value: [f32; 3]) {
        unsafe {
            if let Some(loc) = gl.get_uniform_location(self.id, name) {
                gl.uniform_3_f32(Some(&loc), value[0], value[1], value[2]);
            }
        }
    }

    pub fn uniform_mat4(&self, gl: &glow::Context, name: &str, value: &[f32; 16]) {
        unsafe {
            if let Some(loc) = gl.get_uniform_location(self.id, name) {
                gl.uniform_matrix_4_f32_slice(Some(&loc), false, value);
            }
        }
    }
}

fn compile_shader(gl: &glow::Context, stage: u32, source: &str, label: &str) -> Result<(), String> {
    unsafe {
        let shader = gl.create_shader(stage).map_err(|e| e.to_string())?;
        gl.shader_source(shader, source);
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            return Err(format!(
                "{label}: shader compile failed:\n{}",
                gl.get_shader_info_log(shader)
            ));
        }
        gl.delete_shader(shader);
        Ok(())
    }
}

/// Compile a fragment shader source with the offline context (debug helper).
pub fn compile_fragment_source(
    gl: &glow::Context,
    source: &str,
    label: &str,
) -> Result<(), String> {
    compile_shader(gl, glow::FRAGMENT_SHADER, source, label)
}

/// Compile-only check used for the pack's vertex shaders, which are quoted
/// through OptiFine/Iris built-ins (`ftransform`, `gl_TextureMatrix`).
pub fn compile_vertex_source(gl: &glow::Context, source: &str, label: &str) -> Result<(), String> {
    compile_shader(gl, glow::VERTEX_SHADER, source, label)
}

pub struct FullscreenQuad {
    pub vao: glow::VertexArray,
    #[allow(dead_code)]
    buffer: glow::Buffer,
}

impl FullscreenQuad {
    pub fn new(gl: &glow::Context) -> Self {
        // One oversized triangle in NDC; texture coordinates are derived from
        // the position in the vertex shader.
        let positions: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        unsafe {
            let vao = gl.create_vertex_array().expect("create vertex array");
            let buffer = gl.create_buffer().expect("create vertex buffer");
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck::cast_slice(&positions),
                glow::STATIC_DRAW,
            );
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            gl.bind_vertex_array(None);
            Self { vao, buffer }
        }
    }

    pub fn draw(&self, gl: &glow::Context) {
        unsafe {
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_vertex_array(None);
        }
    }
}

/// Bind a texture to a sampler uniform and a texture unit.
pub fn bind_sampler(
    gl: &glow::Context,
    program: &GlProgram,
    name: &str,
    unit: u32,
    texture: &GlTexture,
) {
    program.uniform_i32(gl, name, unit as i32);
    unsafe {
        gl.active_texture(glow::TEXTURE0 + unit);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture.id));
    }
}
