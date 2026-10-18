# 行为优先的动态天气系统（实现契约）

本文是 `sky-weather` 和 `sky-weather-fx` 的契约。**它不是物理天气模拟或预报**：它用快速二维场、状态机和屏幕空间效果，完整覆盖一套局地天气目录，并保持温度、云、降水、雾、风和雷电之间的近似一致性。

## 范围

`sky-weather` 是 CPU 行为核心：

- 20 种局地天气状态（目录见下）；
- `Auto` 自动演变、`Fixed` 固定、`Tour` 轮巡、`Timeline` 关键帧；
- 128×128 二维场（可配），半拉格朗日/一阶迎风平流 + 目标松弛；
- 移动天气系统：锋面、对流单体、雨带、雪带、雾团、沙尘羽；
- 输出三维云体积、`WeatherFrame` 和供 GPU 使用的 `EffectsState`；
- 昼夜、季节、气候（热带/温带/大陆/干旱/极地/高山）；
- 确定性 seed、snapshot/restore。

`sky-weather-fx` 是独立 GPU 效果核心：

- 屏幕空间雨、雪、冰雹、雨夹雪；
- 雾、霾、沙尘、风暴暗化、彩虹、闪电；
- 零效果逐像素复制背景；
- 不依赖任何其它求解器，只接受通用相机/太阳/强度输入。

**明确不做**：全球/区域 NWP、预报、资料同化、集合、气候预测；BOMEX/RICO/DYCOMS 物理验证；严格守恒；真实地形、地面积雪/积水、台风/飓风/龙卷物理；音频雷声、VR、Minecraft 移植。

## 天气目录

| `WeatherKind` | 行为 |
|---|---|
| `Clear` | 晴空，能见度 >30 km |
| `Fair` | 10–25% 淡积云 |
| `PartlyCloudy` | 25–60% 积云/层积云 |
| `Overcast` | 80–100% 层云 |
| `Fog` | 接地雾，能见度 50–500 m |
| `Haze` | 干霾，能见度 2–10 km |
| `Dust` | 沙尘，强风、低能见度 |
| `Drizzle` | 毛毛雨，0.1–1 mm/h |
| `Rain` | 1–10 mm/h |
| `HeavyRain` | 10–50 mm/h |
| `TropicalShower` | 暖性阵雨，可能彩虹 |
| `Thunderstorm` | 积雨云、闪电、阵风 |
| `Hailstorm` | 雨+冰雹、强闪电 |
| `Supercell` | 风格化强风暴、大冰雹、强阵风 |
| `LightSnow` / `HeavySnow` / `Blizzard` | 冷性降雪，暴风雪含强风 |
| `Sleet` / `FreezingRain` | 雨夹雪/冻雨 |
| `Gale` | 大风，15–30 m/s |

修饰：昼夜、季节、气候、彩虹、阵风锋。可选极端层（首版不做）：`Tornado`、`Hurricane`。

一致性规则：

- 雪 `T<2°C`；雨夹雪 0–4°C；冻雨近地面 `T<0°C`；
- 雾要求高 RH/低风；沙尘要求干/风大；
- 降水必须有云；晴空不能降水；
- 冰雹允许穿过暖地面层；热带气候不支持雪/冻雨，自动降级为雨；
- 固定模式强制不可能组合时降级并在 `Diagnostics` 标出，不静默渲染矛盾状态。

## 坐标与输出

- 水平网格 `x/z`，周期边界；`dx=250 m`、`128×128` 默认覆盖 32 km；`y` 为海拔。
- 云体积 `[128,96,128]`，`origin=[-16000,0,-16000]`，`extent=[32000,14000,32000]`（水平体素 250 m、垂直 146 m）；可在 `Config` 修改。体积重建按 z 切片并行（最多 8 线程），同 seed 下逐位一致；重建与 f16 编码现在由工作台的后台工作线程执行，渲染线程只剩校验 + `write_texture`（异步前后首次上传帧 14–17 ms → 约 1.4–2.0 ms，见[模拟速度与帧率](#模拟速度与帧率)）。
- 体积 `[LWC kg/m³, r_eff m, ice_fraction, cloud_fraction]`，x 最快，行主序。
- 太阳方位约定：0° 朝 +Z，90° 朝 +X；与 `sky-realtime` 一致。
- `WeatherFrame` 是 HUD/效果输入，不是气象分析；`EffectsState` 已钳制到 0–1。

## 公开 API

```rust
pub enum WeatherKind { /* 20 variants */ }
pub enum WeatherMode { Auto, Fixed(WeatherKind), Tour, Timeline(Vec<WeatherKeyframe>) }
pub enum Climate { Tropical, Temperate, Continental, Arid, Polar, Alpine }
pub enum PrecipitationKind { None, Rain, Snow, Sleet, Hail, FreezingRain }
pub enum EffectsQuality { Low, High }

pub struct Config {
    pub mode: WeatherMode,
    pub climate: Climate,
    pub season: f32,          // 0 winter .. 1 summer
    pub intensity: f32,       // 0.25..2.5
    pub grid: [u32; 2],
    pub dx_m: f64,
    pub dt_s: f64,
    pub seed: u64,
    pub latitude_deg: f64, pub longitude_deg: f64,
    pub day_of_year: f64, pub start_hour_utc: f64,
    pub auto_sun: bool,
    pub sun_elevation_deg: f64, pub sun_azimuth_deg: f64,
    pub volume_dims: [u32; 3], pub volume_extent_m: [f64; 3],
    pub effects_quality: EffectsQuality,
}

impl Config {
    pub fn fixed(kind: WeatherKind) -> Self;
    pub fn auto(climate: Climate) -> Self;
    pub fn validate(&self) -> Result<()>;
}

impl Model {
    pub fn new(config: Config) -> Result<Self>;
    pub fn step(&mut self) -> Result<()>;
    pub fn advance(&mut self, seconds: f64) -> Result<()>;
    pub fn time_s(&self) -> f64;
    pub fn config(&self) -> &Config;
    pub fn set_mode(&mut self, mode: WeatherMode) -> Result<()>;
    pub fn set_weather(&mut self, kind: WeatherKind) -> Result<()>;
    pub fn set_intensity(&mut self, intensity: f32) -> Result<()>;
    pub fn sun(&self) -> SunPosition;
    pub fn set_sun(&mut self, elevation_deg: f64, azimuth_deg: f64) -> Result<()>;
    pub fn diagnostics(&self) -> WeatherFrame;
    pub fn frame(&self) -> WeatherFrame;
    pub fn effects(&self) -> EffectsState;
    pub fn sample_volume(&self) -> Result<CloudVolume>;
    pub fn snapshot(&self) -> Snapshot;
    pub fn restore(&mut self, snapshot: &Snapshot) -> Result<()>;
}
```

`advance(seconds)` 以 `dt_s` 固定步长累加，最大 600 s/次；不足一步的余量保留，因此帧率无关。`step()` 等价于 `advance(dt_s)`。

## 行为引擎

### Director

- `Auto`：带驻留时间的 Markov 选择，按气候/季节偏置；热带多阵雨，极地多雪，干旱多沙尘；
- `Tour`：每 18 s 轮换目录；
- `Fixed`：锁定状态，但内部场继续演变；
- `Timeline`：关键帧按模拟时间切换。
- 天气系统有位置、速度、半径、寿命、强度；随背景风移动并周期回卷；系统影响局部云量/降水/雾/闪电。
- 目标场按 20–120 s 混合，避免天气量突然跳变。

### Field

- 每列：温度、湿度、云水、云量、云底/顶、`r_eff`、冰相、雨强、雾、沙尘、能见度、风、闪电潜势、彩虹、暗化。
- 平流后向 director 目标松弛；不做守恒/动量方程。
- 三维云体积按 7 种垂直廓线（无、积云、层云、深对流、雪、卷云、雾）重建；LWP 归一化保持与列一致。覆盖率掩膜的过渡带从 0.14 收窄到约 0.035（高覆盖时更窄），使积云从整列纱幕变成离散云块；云顶/云底叠加低频 3D hash 起伏（顶部约 ±550 m）并沿风向加入高度剪切，让轮廓不再是平顶/垂直帘幕。噪声 octave 现在按 250 m/146 m 体素带宽限制（每波长 ≥ 约 4 体素；移除了此前 750/520 m 的欠采样 octave，阈值化后不再出现体素格纹），更细的次网格结构由 `sky-clouds` 的四级 detail 噪声提供（距离 LOD、仅薄样本侵蚀、天空遮挡与云底变暗；工作台平衡/高质量档 detail_strength 0.8/0.9，见[实时云输运](clouds_realtime.md#子网格细节侵蚀工作台质量档默认开启)）。

### 降水/雾/雷电

- 降水类型与强度来自 `WeatherKind` 目标和一致性规则；雨/雪/冰雹/雨夹雪进入 `EffectsState`。
- 雾/霾/沙尘降低能见度；沙尘要求干风。
- 闪电率由目标风暴强度和局地系统影响决定；GPU 效果使用确定性 flash 包络和 seed。

## 数值与失败模式

- 二维一阶迎风/半拉格朗日平流，周期边界；不做 CFL 物理论证，但每步检查非有限/负水。
- 温度 −55..55°C、湿度 0.01..1、云水 ≤2.2e-4 kg/kg、雨强 ≤150 mm/h、风 ≤45 m/s；越界钳制并记录。
- 非有限场/体积尺寸不匹配/非法 snapshot 返回错误，不静默。
- 同 seed/config 逐位确定；snapshot 包含 RNG/系统/director 状态，恢复后继续同一序列。

## 测试与验收

- `cargo test -p sky-weather`：kind/director/field/snapshot/volume/热力学测试。
- `cargo test -p sky-weather-fx`：配置、输入钳制、层预算、零效果、camera basis、uniform 布局、naga WGSL 解析/验证/SPIR-V。
- `cloud-audit weather-catalog`：20 种状态各推进 120 s，输出 frame/effects/volume/finite。
- `cloud-audit weather-simulate --climate <c> --hours 6`：Auto 转换记录。
- `sky-demo --experiment weather --snapshot ...`：行为截图；CLI 支持 `--snapshot-weather-mode/kind/climate/seed/spinup-s`。
- `cloud-audit effects-zero --size 64`：GPU 零效果恒等式；当前 `max_abs_error=0.0`。
- 视觉目标：1080p 交互 60 fps；fx 层 ≤1.5 ms；零效果逐像素等于背景。

本机首批截图见 `out/weather-*.png`；数值是行为诊断，不是观测认证。

### 本机实测（2026-10-17 首测，2026-10-18 更新测试与体积数据；Windows / AMD Radeon 8060S）

- `cargo test -p sky-weather`：49 个测试通过（kind/director/field/snapshot/volume/并行重建一致性/克隆快照逐位一致/热力学/旧模块回归）。`cargo test -p sky-clouds`：23 个测试通过；`cargo test -p sky-demo`：31 个（含工作台异步体积管线与连续云运动（漂移插值/自适应重建间隔）的 4 个新测试）。`cargo test -p sky-weather-fx`：17 个测试通过（含 naga WGSL/SPIR-V、uniform 布局、零效果 CPU 恒等式）。
- `cargo run --release -p cloud-audit -- weather-catalog --seconds 120`：20/20 状态 `finite`，雪/雨夹雪/冻雨温度与降水类型一致，冰雹不被暖地面层降级。
- `cargo run --release -p cloud-audit -- weather-simulate --climate temperate --hours 6 --chunk-s 300`：21600 s，61 次状态转换，覆盖 Clear/Fair/Overcast/Fog/Haze/Drizzle/Rain/HeavyRain/Thunderstorm/Blizzard/Supercell 等。
- `cargo run --release -p cloud-audit -- effects-zero --size 64`：GPU 零效果 `max_abs_error=0.0`。
- `cargo run --release -p cloud-audit -- benchmark`（128² 网格、128×96×128 体积）：行为步进约 **1.63–1.74 ms/步**，体积采样约 **13.6–15.1 ms/次**（8 线程并行，多次运行波动；旧 96×64×96 约 9.78 ms）；本机 CPU，不含渲染/I/O。工作台里这些采样现在跑在后台工作线程，渲染线程只做上传。
- 应用截图：`out/weather-fair-fx.png`（积云）、`out/weather-rain-tuned.png`（雨）、`out/weather-snow-tuned.png`（雪）、`out/weather-fog.png`（雾）、`out/weather-storm.png`（雷暴雨）。
- 尚未做：1080p 长时间帧率测量、全部 20 状态的自动截图对比、真实数据驱动。

## 可视化工作台（实时）

`apps/sky-weather-workbench` 是天气/云的实时可视化工作台入口。它复用 `sky-demo` 的显示外壳（GPU 上下文、egui 集成、实验抽象与呈现 pass），在应用层组合 `sky-weather`（行为天气）、`sky-realtime`（四波长天空）、`sky-clouds`（实时体积云输运）与 `sky-weather-fx`（屏幕空间效果）；应用只负责参数、I/O、展示与实验统计，不实现输运、天气行为或效果合成。

它是行为优先的可视化工作台，**不是**天气数据查看器、预报或气象分析工具；20 种状态是可视行为目录，不是观测统计。核心 crate 之间仍互不依赖，管线顺序与合成归应用层。中文界面只覆盖本入口；PT 对照工作台 `sky-demo` 的界面保持原有英文。

### 入口与命令

```powershell
# 交互工作台：默认自动天气（Auto）；中文界面，窗口标题「天气可视化工作台」。
cargo run --release -p sky-weather-workbench
# 等价的 Windows 统一入口（仓库根目录）：.\run.cmd workbench

# 固定天气（20 种之一）；--kind 在 --mode auto 下隐含 fixed 模式。
cargo run --release -p sky-weather-workbench -- --mode fixed --kind thunderstorm
# 轮巡 20 种目录（每 18 s 轮换）。
cargo run --release -p sky-weather-workbench -- --mode tour
# 指定种子/气候/季节/站点太阳时间；auto sun 由天气时钟驱动太阳。
cargo run --release -p sky-weather-workbench -- --mode auto --climate polar --seed 42 --season 0.9 --latitude-deg 68 --day-of-year 355 --start-hour-utc 13

# 冒烟截图：无人工操作，离屏渲染真实管线 + UI，写一张 PNG 后自动退出。
cargo run --release -p sky-weather-workbench -- --smoke 24 --warmup-seconds 120 --screenshot out/workbench-weather-smoke.png --width 1600 --height 900
```

`cargo run --release -p sky-weather-workbench -- --help` 是选项、默认值与枚举取值的最终来源（帮助文本为中文，枚举取值仍保留英文 CLI 名称）；下表只解释语义，不替代 `--help`。

### CLI 选项

|选项|默认|语义|
|---|---|---|
|`--mode <auto\|fixed\|tour>`|`auto`|auto 自动演变；fixed 锁定 `--kind` 状态（内部场继续演变）；tour 每 18 s 轮换 20 种目录。|
|`--kind <名称>`|未指定|固定天气（20 种之一，见上文[天气目录](#天气目录)）；`--mode auto` 时隐含切到 fixed。名称不合法时启动报错。|
|`--climate <tropical\|temperate\|continental\|arid\|polar\|alpine>`|`temperate`|行为模型的气候输入；热带多阵雨、极地多雪、干旱多沙尘。|
|`--intensity <0.25..2.5>`|`1.0`|天气强度倍率（作用于行为目标量）。|
|`--speed <1..600>`|`60`|每墙钟秒推进的模拟秒数。|
|`--seed <u64>`|程序常量|确定性 seed；同 seed/配置逐位可复现（snapshot 语义见前文）。|
|`--season <0..1>`|`0.55`|行为季节：0 冬、0.5 春/秋、1 夏；不是观测季节。|
|`--latitude-deg <度数>`|`45`|天气时钟太阳位置使用的纬度（Michalsky 1988）。|
|`--day-of-year <1..366>`|`172`|天气时钟太阳位置的年积日。|
|`--start-hour-utc <0..24>`|`9`|天气时钟的起始地方太阳时；之后按 `--speed` 前进。|
|`--auto-sun` / `--no-auto-sun`|开启|auto 由天气时钟驱动太阳高度/方位；`--no-auto-sun` 使用手调太阳值。auto sun 开启时手调值被忽略。|
|`--sun-elevation-deg <度数>`|`30`|手动太阳高度（仅 `--no-auto-sun`）。|
|`--sun-azimuth-deg <度数>`|`180`|手动太阳方位（0° 朝 +Z、90° 朝 +X，与 `sky-realtime` 一致；仅 `--no-auto-sun`）。|
|`--observer-altitude-km <km>`|`0.2`|观察海拔。|
|`--yaw-deg` / `--pitch-deg` / `--fov-y-deg`|`0` / `18` / `70`|初始相机水平/俯仰/垂直视场角；窗口内可拖拽、滚轮修改。|
|`--clouds <balanced\|high>`|`balanced`|体积云质量：balanced 0.8× 追踪（1600×900 → 1280×720）、high 0.85× 追踪（→1360×765）。|
|`--effects <high\|low>`|`high`|屏幕空间天气效果质量。|
|`--exposure-ev <EV>`|`0`|显示曝光补偿（相对校准曝光，每 EV 翻倍/减半）。|
|`--cloud-occlusion <0..1>`|`1`|云层遮光强度：按云量/含水量压暗天空、地面与云底，并淡出太阳盘；0 关闭该耦合。|
|`--warmup-seconds <模拟秒>`|`90`|首帧前推进天气模拟的预热时长；交互与截图路径都生效。|
|`--smoke <FRAMES>`|未启用|冒烟模式：渲染 N 帧离屏画面后写 PNG 退出（1..=10000）。|
|`--screenshot <PATH>`|无|冒烟截图输出路径；`--smoke` 必需。|
|`--width` / `--height`|`1600` / `900`|离屏截图尺寸（仅冒烟；最小 320x200），与窗口大小无关。|

### 工作台内配置（设置面板）

窗口右侧“设置面板”是同一组输入的中文交互版本；每节标题可点击“复位”恢复该节：

- **天气与云**：体积云与天气效果开关；快捷预设（晴空/多云/雨/雷暴/雪/雾）；天气模式（自动/固定/目录轮巡）；固定天气目录（20 种）；气候；强度；模拟速度（对数滑杆）；暂停；种子；自动太阳；云质量；效果质量；**云层遮光强度**（0=不遮光，1=按云量/含水量自动压暗天空、地面与云底）。
- **季节与站点**：季节 0..1、纬度 -90..90、年积日 1..366、起始 UTC 小时 0..24；自动太阳开启时由这些值驱动太阳。
- **太阳与观察者**：手动方位 0..360、高度 -90..90（自动太阳开启时禁用）、观察海拔；点击节标题会关闭自动太阳并复位手调值。
- **相机**：偏航/俯仰/垂直视场角滑杆与复位；视口内拖拽=视角、滚轮=缩放。
- **显示**：曝光 EV、色调映射、Reinhard 渐近线、HDR 纸白亮度/峰值钳制；HDR 输出由工具栏开关或 F6 切换（适配器支持 scRGB 时）。

窗口按键：拖拽=视角，滚轮=缩放，空格=暂停，`[` `]`=太阳高度，`,` `.`=太阳方位，F6=HDR（支持时），R=重置视角。界面中文字体在启动时从系统加载（Noto Sans SC / 等线 / 微软雅黑等回退）；找不到中文字体时会打印警告并可能显示方框。

### 诊断 HUD 与状态栏

- 视口左上角 HUD：天气种类 + 模拟时间（暂停时标「已暂停」）；气温 / 湿度 / 云量 / 能见度；太阳方位/高度、LWP、风速/风向、降水类型与雨强、闪电率；底部是拖动/滚轮/空格等交互按键提示。
- 视口右上角：帧率与实验名（冒烟截图时显示「帧率 N fps（采集）」）。
- 底部状态栏（weather 模式）：天气种类、模拟时间、气温、湿度、云量、LWP、云底/云顶、降水类型与 mm/h、风、能见度、闪电率；右侧帧率与 SDR/HDR 状态；模型未就绪时显示 `等待天气诊断…`。
- 窗口标题每秒附加一次状态摘要（`天气可视化工作台 | ...`）。

这些数值来自行为模型诊断（`WeatherFrame`/`WeatherHud`），不是气象分析产品。

### 渲染管线与节奏

每帧由应用层显式组合（均通过各核心公开 API）：

```text
sky-realtime（四波长天空、太阳/天空查找）
  -> sky-clouds（体积云 raymarch 与天空合成）
  -> sky-weather-fx（屏幕空间雨/雪/雾/沙尘/雷电/彩虹）
  -> present（曝光、Enhanced Reinhard + Oklab 色域映射、SDR/HDR 呈现）

天气诊断 HUD 与 egui 界面在呈现前叠加。
```

云质量「平衡」档以 0.8× 分辨率追踪体积（1600×900 时 1280×720，12 样本 / 32 步），再用时间累积（TAA，旋转重投影 + 邻域钳制）和 5×5 双边滤波上采样合成；「高质量」档 0.85×（1360×765，16 样本 / 40 步），同样走时间累积。追踪分辨率是单一旋钮 `trace_scale`（0.25..1.0），不再是 half/full 布尔值。两档都开启子网格细节侵蚀（平衡 0.8、高质量 0.9）、Beer-Powder、12 km 空气透视与 512² 地面云影 map；天气体积 128×96×128 并按 z 切片并行重建（重建与编码在后台线程，渲染线程只上传）；高质量档的太阳阴影 march 为 10 步，平衡档 6 步。本机 1600×900 捕获中，平衡档 fair / partly / thunderstorm / fog 中位约 4.6–10.6 ms；高质量档 fair 约 8.0 ms、partly 约 13.1 ms；1080p 平衡 partly 约 12.1 ms。本轮把追踪分辨率从 0.65/0.8 提高到 0.8/0.85，云缘的 per-trace-pixel 阶梯明显减少，代价是平衡档成本约 +70–90%（见[实时云输运](clouds_realtime.md)）。

阴天/雷暴的天空、地面和太阳盘不只来自云体积遮挡：应用层还把天气诊断换算成两个 0..1 系数，`sky-clouds::Renderer::set_overcast` / `set_storm_severity` 传给复合与云光照，同时通过 `sky-realtime::Renderer::sun_disk_gain` 淡出太阳盘：
- **阴天遮挡**（云量 × 液水路径）：压暗背景天空/地面与云辐射、向中性灰混色；云体积 pass 在遮挡越强时越淡出直射太阳项与天空环境光；太阳盘亮度乘以 `1 - 1.15·occlusion` 后钳制到 0。普通阴天/降雨保持白昼灰。
- **对流风暴强度**（闪电率、冰雹、>30 mm/h 的雨强）：只对雷暴/冰雹/超级单体再叠一层近暗压降（背景 ×0.30、云辐射 ×0.34），并加强云底低频结构与底暗梯度，做出「黑云压城」的观感；普通阴天不触发。
- 工作台「云层遮光强度」滑杆（或 CLI `--cloud-occlusion`，0..1）缩放这两个系数；默认 1.0，设为 0 可回到不做阴天压暗、太阳盘不额外淡出的旧观感。
- 为增强压迫感，本轮还把雷暴类天气的目标云底调低（雷暴 700→450 m、冰雹 600→420 m、超级单体 500→380 m 等），云底更贴近观察者；这仍由行为目录驱动，不是实测风暴结构。
- 这是**应用层近似**，不是输运云影或辐射传输：它让雷暴/阴天看起来更暗、云底更灰、厚云下的太阳盘消失，也没有云-天空双向反馈。地面云影由 `sky-clouds` 的 top-down shadow map 单独提供（单次散射、仅 y=0 地面）；真实的云层遮蔽/多次散射仍是显式差距。

交互模式在天气未暂停时约每 16 ms 请求重绘（约 60 Hz 的节奏目标），模拟时间按 `--speed` 相对墙钟推进，与帧率的关系见[模拟速度与帧率](#模拟速度与帧率)；实际帧率取决于 GPU、分辨率、云质量与效果质量（见[本机验证](#本机验证)）。

### 模拟速度与帧率

工作台里有两个时钟，只有后者受「模拟速度」控制：

| 时钟 | 来源 | 决定什么 |
|---|---|---|
| 墙钟 | `Instant`，每帧测一次帧间隔 | 这一帧推进多少模拟秒、体积云多久更新、HUD 帧率 |
| 模拟 | `sky_weather::Model::time_s`，按固定步长 `dt_s` 累积 | 天气/云/降水/闪电状态、自动太阳、HUD 的 `t` |

应用层每帧的换算只有一处（`apps/sky-demo/src/passes/weather.rs`）：

```text
wall_step   = clamp(now - last_step, 0, 0.1 s)
sim_seconds = wall_step * speed     // speed = --speed / 「模拟速度」，1..600
model.advance(sim_seconds)          // 固定步长累加器
```

`Model::advance(seconds)` 把 `seconds` 加进累加器，只消费完整的 `dt_s = 1 s` 步，不足一步的余量留给下一帧（`step()` 只是 `advance(dt_s)`）。由此：

- **帧率不改变长跑模拟速率。** 30/60/144 fps 只改变模型步落在哪一帧，长跑仍是 `speed` 模拟秒/墙钟秒；帧率带来的唯一差别是小于 `dt_s = 1 s` 的相位滞后。
- 模拟时间是 `dt_s` 的阶梯：状态只在步边界变化；两帧之间通常重复同一状态（帧率高）或一帧跨过多个状态（帧率低或速度高）。
- 固定步长也决定动画的时间分辨率：模型状态更新频率为 `speed / dt_s` 次/墙钟秒。`--speed 1` 时天气/雨雪动画每秒才变一次，`--speed 60` 时与 60 fps 目标对齐，`--speed 600` 时每帧跨多个状态。

#### 谁决定什么

| 量 | 决定者 | 关系 |
|---|---|---|
| 模拟秒 / 墙钟秒 | `--speed` | `speed × min(1, 0.1 s / 帧间隔)` |
| 模型状态更新频率（墙钟） | `--speed` | `speed / dt_s` 次/秒 |
| 画面采样频率 | 帧率 | 高于更新频率时重复帧，低于时跳过状态 |
| 每帧推进的模拟秒 | speed ÷ fps | 60× / 60 fps = 1 s；600× / 60 fps = 10 s |
| 云体积内容更新 | 墙钟定时器 + 模型时间 | 最早每 `clamp(60 m ÷ (风速 × 模拟速度), 0.10, 0.28) s` 检查一次；只有天气时钟真的前进才重建（speed=1 受 1 s 模型步限制约 1 Hz，speed=60 约 5–6 Hz，上限 10 Hz）；重建/编码在后台线程，渲染线程只上传（约 1.4–2.0 ms） |
| 云的运动（画面内） | 帧率 + 体积漂移插值 | 每帧按“风矢量 × 快照后经过的模拟秒数”平移 3D 体积采样位置；新快照上传时归零，因此两次重建之间云仍连续移动，不会按重建频率跳帧 |
| HUD `t` 与天气诊断 | 同一次体积上传 | 最多滞后一个体积更新间隔 |

渲染帧率与模拟速度解耦：滑块只改变“每秒模拟多少秒天气”，不改变 vsync/GPU 的呈现帧率。但体积云内容以前每 280 ms 必重建一次；`speed=1` 时天气模型按 1 s 固定步长推进，多数重建读到的还是同一状态，于是每 280 ms 白做一次 13.6 ms 的 CPU 重建 + 上传，表现为周期性掉帧/画面“一顿一顿地闪”。上一轮已改为“只有天气时钟真的前进才重建”，并用连续模拟钟驱动细节噪声漂移、把静止 TAA 混合系数从 0.12 降到 0.08；本轮再把重建 + f32→f16 编码整体搬到后台工作线程，渲染线程只剩校验 + `write_texture`（约 1.4–2.0 ms；异步前同一帧 14–17 ms）。`speed=60` 的体积更新尖峰从 **49–54 ms 降到约 10 ms**（120 帧捕获中位 3.3 ms、P95 6.2–6.6 ms、最大 9.7–10.1 ms），`speed=1` 的 90 帧捕获 P95 3.8 ms、最大 4.3 ms。

云的**运动**也不再跟着体积更新频率跳：`sky-clouds` 新增 `Renderer::set_volume_drift`，渲染端每帧按“风矢量 × 上次体积快照以来经过的连续模拟秒数”平移 3D 体积采样位置，新快照上传时漂移归零。这样即使体积只有约 5–6 Hz 重建，画面里的云仍逐帧平滑移动，模拟速度只决定时间加速倍率（风吹云的物理速度 = 风速 × 模拟速度，60× 即延时摄影），不再决定运动是否平滑。重建间隔也随模拟速度自适应收紧（见上表），保证每次更新时烘焙的位移不超过约 60 m。顺带修正了体积噪声的平流方向：此前噪声场向“风来向”漂移，与 detail 噪声和 HUD 风向相反；现在两者都统一为顺风漂移。

#### 低帧率会丢掉一部分模拟时间

`wall_step` 被钳制在 0.1 s（`MAX_WALL_STEP`），所以帧间隔超过 100 ms（低于 10 fps）时有效速率变成：

```text
有效速率 = speed × 0.1 s / 帧间隔 = speed × fps / 10
```

| 实际帧率 | `--speed 60` 的有效速率 |
|---:|---:|
| 10 fps | 60× |
| 5 fps | 30× |
| 2 fps | 12× |
| 1 fps | 6× |

窗口拖动/缩放、最小化、4K/高云质量、debug 构建或一次长卡顿都会触发；被丢掉的墙钟时间不补，避免卡顿后瞬间跳很多模拟步。暂停、关闭天气或进入错误状态时模拟完全停止，恢复后从原处继续，同样不补时间。

#### 反过来：速度也会压低帧率

每一步都是 CPU 工作。本机 128×128 网格、128×96×128 体积的实测约 **1.63 ms/步**（`cloud-audit benchmark`）。于是：

- 每墙钟秒的模型 CPU ≈ `speed × 1.69 ms`；`--speed 60` 约占单核 10%，600× 约 101%。
- 每帧的模型 CPU ≈ `speed / fps × 1.69 ms`；60 fps 下 60× 约 1.7 ms/帧，600× 约 16.9 ms/帧——已经超过 16.7 ms 的 60 fps 预算。
- 于是形成反馈：帧变慢 → 每帧要补的模拟秒变多 → 模型步变多 → 帧更慢。0.1 s 钳制把最坏情况限制为每帧最多 `0.1 × speed` 模拟秒，但模型时间单独就能把帧率压到 `1 / (0.1 × speed × 1.69 ms)` 附近（600× 时约 9.9 fps；加渲染后实测约 9 fps）。

体积云采样是另一个固定成本：约 **13.6–15.1 ms/次**，只在天气时钟前进时重建（speed=1 约 1 Hz，speed=60 约 5–6 Hz）。它在后台工作线程执行，渲染线程只做校验 + 上传（约 1.4–2.0 ms），因此不再在渲染线程上形成 13 ms 级周期尖峰。异步前后 speed=60 的 120 帧捕获最大帧时间 49–54 ms → 9.7–10.1 ms；speed=1 的 90 帧捕获 P95 3.8 ms、最大 4.3 ms。两次重建之间，云的运动由渲染端体积漂移插值逐帧补齐，不会出现“一帧一帧”的停格。

#### 本机实测（2026-10-18）

`sky-weather-workbench`，`--mode fixed --kind rain --clouds balanced --effects high --warmup-seconds 30 --smoke 90 --width 960 --height 540`，未限帧捕获（不是交互帧率）：

| `--speed` | 中位帧时间 | 折算吞吐 | 每帧模拟秒 | 有效模拟速率 |
|---:|---:|---:|---:|---:|
| 1 | 1.59 ms | 631 fps | 0.002 s | 1× |
| 60 | 1.68 ms | 594 fps | 0.10 s | 60× |
| 120 | 5.73 ms | 175 fps | 0.69 s | 120× |
| 300 | 11.36 ms | 88 fps | 3.4 s | 300× |
| 600 | 110.87 ms | 9 fps | 60 s（钳制） | ≈540× |

- 600× 那行正是 0.1 s 钳制的平衡点：每帧 60 步 × 1.69 ms ≈ 101 ms 模型时间 + 约 10 ms 渲染 = 110 ms；请求的 600× 只兑现约 540×。
- 300× 的帧时间增量约 5.8 ms，与每帧约 3.4 步 × 1.69 ms 相符；120×/60× 的增量也分别与 0.69/0.10 步相符。
- 捕获路径不按 16 ms 节奏、也不等垂直同步；交互模式的绝对帧率会不同（还受 UI 与显示刷新限制），但「速度越高 → 每帧模型步越多 → 帧时间越长」的关系相同。

#### 对复现的含义

- `--warmup-seconds` 是墙钟无关的模拟秒偏移，预热后的状态由 seed/模式/气候/预热共同确定。
- 冒烟捕获期间模型仍按 `speed × 实际帧时间` 前进，所以同一 seed/预热下，捕获结束时的模拟时间仍会随机器和负载变化（60×、90 帧在本机约多走 16 模拟秒，慢机器可能更多）。需要逐位比较的状态请用 `cloud-audit` 的固定 `advance` 路径；屏幕截图只用于视觉回归。
- 想减小捕获漂移可用 `--speed 1`（捕获期间通常跨不过一个模型步），或固定预热后立即截图。
- 修改天气模式/种类/seed/气候/强度/季节/站点会重建模型并从 `t = 0` 开始；切换 HDR 或重新加载资产会重建实验并重新执行预热，`t` 会退回 `--warmup-seconds`。

### 截图模式（--smoke）

`--smoke <FRAMES>` 不做交互会话：按 `--width/--height` 创建隐藏窗口与离屏渲染目标，用与交互路径相同的 experiment/UI 管线渲染 N 帧，读回 RGBA8 写一张 PNG，打印稳态帧时间统计（中位/均值/最小/P95/最大，排除首帧的管线与首次资源上传）后自动退出；HUD 的帧率徽标会标注「（采集）」以区别于交互帧率。`--warmup-seconds` 在首帧前推进天气行为，避免用初始场截图。截图是 SDR 显示图（不是线性/HDR 辐射度量数据），不能单独作为物理正确性或性能结论的依据。预热与初始状态由 `--seed`、模式、气候和 `--warmup-seconds` 共同确定；但捕获期间模型仍按 `--speed` × 实际帧时间推进，最终模拟时间还会随机器/负载变化（`--speed 1` 时捕获期间通常跨不过一个模型步）。逐位比较请走 `cloud-audit` 的固定 `advance` 路径，冒烟截图只用于视觉回归。

### 本机验证

以下为 2026-10-17 的基线记录（历史数据）。本轮迭代后的云渲染帧时间见[实时云输运的实测](clouds_realtime.md)，体积重建/上传与模拟速度-帧率关系见上文 本机实测 与[模拟速度与帧率](#模拟速度与帧率)。冒烟截图路径离屏渲染**真实管线 + UI** 后自动退出；帧时间是**未限帧**的稳态捕获吞吐（排除首帧的管线创建、首次云体积上传与首次 SkyView 重建），不是开启垂直同步时的交互帧率承诺：

| 场景（捕获命令参数） | 分辨率 | 稳态帧时间中位数 | 折算 | 截图 |
|---|---:|---:|---:|---|
| 默认 Auto，warmup 120 s | 1600×900 | 4.8–10.0 ms（多次运行） | 100–207 fps | `out/qa3/wb-auto.png` |
| 雷暴 Fixed，warmup 180 s | 1280×720 | 2.6–3.1 ms | 325–390 fps | `out/qa3/wb-storm-1280.png` |
| 大雪 Fixed + polar | 1600×900 | ≈10 ms | ≈100 fps | `out/qa3/wb-snow.png` |
| 雾 Fixed | 1600×900 | 12.4–13.6 ms | 73–81 fps | `out/qa3/wb-fog.png` |
| 手动太阳 8°/250° | 1600×900 | 4.6–4.8 ms | 208–215 fps | `out/qa3/wb-sunset.png` |
| `--clouds high` | 1600×900 | 15.1–16.0 ms | 62–66 fps | `out/qa3/wb-high.png` |

阴天/雷暴的「遮光」是这一轮的重点，本机对比（同相机、同太阳：`--no-auto-sun --sun-azimuth-deg 105.4 --sun-elevation-deg 47.9 --yaw-deg 0 --pitch-deg 18`，1600×900，取远离 HUD/面板的区域；截图见 `out/qa3/final-*.png`）：

| 天气 | 上部天空平均亮度 | 相对晴空 | 地面平均亮度 | 相对晴空 |
|---|---:|---:|---:|---:|
| 晴空 | 148.0 | 1.000 | 177.3 | 1.000 |
| 阴天 | 101.0 | **0.682** | 144.0 | 0.813 |
| 雨 | 53.9 | **0.364** | 126.0 | 0.711 |
| 雷暴 | 20.2 | **0.137** | 70.3 | **0.397** |
| 强风暴（supercell） | 9.0 | **0.061** | 46.9 | **0.265** |

- 雷暴/强风暴现在压到接近夜色的暗灰黑：上部天空只有晴空的 11–14%，同时压暗地面与云底，并按遮挡淡出太阳盘。太阳面向的 24×24 像素窗口里，晴空 max 255，雷暴 max 25.8、局部对比约 +0.12（round 2 时还有 +131），即厚云下太阳盘不可见；阴天仍保留一点被大幅压暗的亮斑。
- 强风暴再叠一层「深对流」档：应用按 LWP（>2200 g/m²）把风暴强度推到 1.0 以上，composite 额外乘背景 0.50 / 云 0.40；supercell 实测上部天空约 9、地面约 47（相对晴空 0.06 / 0.27），接近夜色，比普通雷暴再暗一档。这是 composite 层的风格化压暗，不是物理亮度；闪电闪光帧会让整帧亮度上升，单帧对比有波动（上表是未闪光帧）。
- 冰雹粒子进一步改为小、硬、带短拖影的冰粒（半径约 0.005–0.010 屏高、部分不透明度混合）；不再像巨大的亮椭球，也不会主导 supercell 画面。
- 遮光强度由「云层遮光强度」滑杆（或 `--cloud-occlusion`，0..1）缩放；0 关闭应用层压暗与太阳盘淡出（天空/地面仍由云体积本身遮挡），1 为默认自动遮光。
- 冷启动到第一张截图约 **4.7–4.9 s**，其中约 3.4 s 是两次固定介质四波长求解（天空 + 环境两个 `Renderer` 初始化）；天气行为预热（`--warmup-seconds`）在同一进程内以 CPU 推进，默认 90 s 模拟。
- 帧时间来自同一台共享显存的 iGPU，运行间波动明显（同一场景重复测量可差约 2 倍），只作数量级参考；云步数、分辨率与云/效果质量影响最大。表格是多次测量的范围，不构成对其他 GPU 或 1080p 长时的性能承诺。
- 交互窗口在 10 s 运行中正常存活（同一 iGPU / Vulkan，无 panic）；窗口帧率受垂直同步、窗口像素尺寸和 DPI 影响，本轮没有做 1080p 长时帧率统计。暂停/恢复与状态栏/HUD 刷新在 10 s 会话中未见异常，但没有做小时级稳定性测试。
- 截图尺寸与请求一致，alpha=255 全帧、非黑像素比例 1.000；截图来自中文界面，HUD 帧率徽标显示「帧率 NNN fps（采集）」，表示这是未限帧捕获吞吐。1280×720 下状态栏按可用宽度截断（以 `…` 结尾），HUD/徽标不重叠。
- 体积云体素约 **250×146×250 m**（128×96×128 over 32×14×32 km），Balanced 用 0.8× 追踪、High 用 0.85× 追踪；云顶/云底有低频起伏并带风向剪切，噪声 octave 已按体素带宽限制，次体素结构改由渲染端四级 detail LOD 提供（见[实时云输运](clouds_realtime.md#子网格细节侵蚀工作台质量档默认开启)）。云缘从旧版的块状/柱状阶梯变成更不规则的花椰菜状，但**远距光学厚云**的轮廓仍受 250 m 体素与 march 步长限制（近处与云层结构改善明显）。地面有 512² top-down 云影 map（4-tap PCF，暗化上限 55%，只在体积覆盖域内生效）；仍没有多次散射云影、云影投到天空或云-天空双向反馈。
- 20 种状态的数值行为仍由 `cloud-audit weather-catalog` 覆盖；本次只抽了代表性场景做自动截图，未做全部目录截图对比、真实数据驱动或 1080p 长时运行。
- 测试（2026-10-18 更新）：`sky-weather` 49 + `sky-weather-fx` 17 + `sky-clouds` 23 + `sky-realtime` 19 + `sky-demo` 31 + `sky-weather-workbench` 5 个用例通过；`cargo check --workspace` 与 `cargo fmt --check`（改动包）通过；`scripts/check_architecture.py` 报 "Architecture OK: 7 independent solvers, 16 Rust packages"。注意本机 `cargo test -p sky-demo` 的 9 个既有用例在默认 `%TEMP%` 下会因 `PermissionDenied` 失败（与本功能无关）；把 `TMP`/`TEMP` 指向工作区内可写目录后 31/31 通过。

## 依赖与所有权

- `sky-weather` 不依赖任何 `sky-*`，不包含 wgpu；只有 serde。
- `sky-weather-fx` 独立 wgpu；不依赖 `sky-*`；应用负责映射 `WeatherFrame/EffectsState`。
- 不得读取 `out/`；数据/参数放在 `crates/sky-weather/` 或 `crates/sky-weather-fx/` 并注明来源。

## 近似依据

- 饱和/露点：Buck (1981) 快速形式；只用于行为温度/湿度一致性。
- 太阳位置：Michalsky (1988)。
- 天气分类参考 WMO 天气现象代码的常见类别；实现值是为可视行为调过的目标，不是观测统计。
- 屏幕空间雨雪雾雷是风格化效果，不是辐射传输或粒子物理；详见 [`weather_fx.md`](weather_fx.md)。
