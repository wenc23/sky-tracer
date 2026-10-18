# sky-weather-fx

Behaviour-first, screen-space weather effects for the sky-tracer applications.
The crate is independent of the atmosphere solvers and of the weather model: the
application supplies weather intensities, camera and sun, and receives a
`Rgba32Float` linear Rec.2020 image that composites the effects over the sky
background.

## API

```rust
pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

pub enum Quality { Low, High }
pub struct Config { pub quality: Quality, pub max_effects: u32 } // Default + validate
pub struct Camera { pub origin_m: [f32; 3], pub yaw_deg: f32, pub pitch_deg: f32, pub fov_y_deg: f32 }
pub struct Sun { pub direction: [f32; 3] }
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
    pub lightning_flash: f32, // 0..1
    pub lightning_seed: u32,
}
pub struct Renderer;
impl Renderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, config: Config) -> Result<Self>;
    pub fn resize(&mut self, device: &wgpu::Device, size: [u32; 2]) -> bool;
    pub fn set_background(&mut self, device: &wgpu::Device, background: &wgpu::TextureView);
    pub fn render(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        input: Input,
    ) -> Result<()>;
    pub fn target_view(&self) -> &wgpu::TextureView;
    pub fn target_texture(&self) -> &wgpu::Texture;
    pub fn stats(&self) -> &Stats;
}
pub struct Stats { pub frames: u64, pub last_output: [u32; 2] }
```

`Input::validate` and `Config::validate` are public. All intensity channels are
clamped to `[0, 1]` after the finite check; non-finite values and a negative
`time_s` are rejected. `lightning_flash` is the application-owned envelope of a
flash event; the procedural bolt is derived from `lightning_seed`.

The background view must be a float-sample texture (typically `Rgba32Float` or
`Rgba16Float`), because the shader reads it with `textureLoad`. The output
texture is `Rgba32Float` and has the size passed to `resize`.

## Screen-space approximations (v1)

- Particles are stateless procedural functions of the pixel position, a
  per-layer hash and `time_s`. There is no particle simulation, no depth buffer,
  no collision, no motion blur and no temporal reprojection or accumulation.
- Rain, sleet, snow and hail are stylised screen-space layers. Depth is faked
  with a per-layer parallax parameter (near layers are larger, faster and
  brighter); nothing is occluded by scene geometry and no layer is attenuated by
  scene depth.
- Fog/haze, dust, cloud darkening, the rainbow and the lightning bolt are
  analytic screen-space terms driven by the view ray. They are not transported
  through a participating medium, and `cloud_darkening` does not cast real
  shadows.
- The rainbow is the primary 42 degree and secondary 51 degree band around the
  anti-solar direction, faded by solar elevation. It is additive light, not a
  spectral dispersion model.
- `max_effects` is a per-pixel layer budget: particle layers are allocated in the
  order rain, snow, hail, sleet. `Quality::Low` requests up to 2/2/1/1 layers,
  `Quality::High` 3/3/2/2, clipped by the budget.
- Zero weather (all intensities 0, flash 0) has an explicit shader short-circuit
  that copies the background exactly; the compositing identity is also covered
  by CPU tests.
- The application owns exposure and tone mapping; the crate writes linear
  Rec.2020 radiance.
