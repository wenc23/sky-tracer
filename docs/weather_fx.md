# 实时天气效果层（`sky-weather-fx`）

`sky-weather-fx` 是独立 wgpu 求解器，只做屏幕空间天气效果。它不知道天气模型或天空求解器；应用把 `sky_weather::EffectsState` 映射成通用 `Input`。

## 输入

```rust
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
    pub lightning_flash: f32,
    pub lightning_seed: u32,
}
```

- 强度在验证为有限后钳制 `[0,1]`；`time_s >= 0`；`fov_y` 在 `(1,170)`；太阳方向非零。
- 背景必须是 float-sample 纹理（`Rgba32Float`/`Rgba16Float`）；错误格式返回错误，不 panic。
- 输出 `Rgba32Float`，线性 Rec.2020，尺寸由 `resize` 决定。

## 效果

- **雨/雨夹雪**：风致倾斜的屏幕空间雨丝；多层预算控制密度/尺度。
- **雪**：带视差的程序化雪花，风向漂移；雪层单独计。
- **冰雹**：小、硬、快的冰粒层，带短垂直运动拖影；以部分不透明度混合，避免变成占据画面的大颗发光椭球（supercell 场景由 QA 标记后调整）。
- **雾/霾/沙尘**：向地平线增强的屏幕空间混合，降低对比度；不是体积介质。
- **风暴暗化**：程序化降低背景亮度/饱和度（屏幕空间图案，不是输运云影）；与 `sky-clouds` 的阴天遮挡压暗叠加时，雷暴/阴天会明显变暗。
- **彩虹**：反太阳方向 42°/51° 主/次弧，随太阳高度淡出；不是光谱色散。
- **闪电**：应用提供的 flash 包络 + seeded 程序化分叉电弧；不模拟放电。
- 所有效果是**风格化屏幕空间近似**，没有深度、遮挡、体积散射或粒子模拟。`cloud_darkening` 的强度范围是行为调参结果，不代表物理透射率。

## 零效果

`rain=snow=hail=sleet=fog=dust=cloud_darkening=rainbow=lightning_flash=0` 时，shader 显式短路并复制背景；当背景和视口尺寸一致时逐像素相等（f32 复制）。单元测试覆盖 CPU 复合恒等式和 naga WGSL 验证。

## 合成顺序

```
sky-realtime → sky-clouds → sky-weather-fx → present/tonemap
```

`sky-clouds` 以天空为背景，`sky-weather-fx` 以云层合成结果为背景；应用负责选择 present source。
