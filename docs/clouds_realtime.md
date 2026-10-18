# 实时云输运与渲染（实现契约）

`sky-clouds` 是独立于大气求解器和天气模型的实时云输运核心。它只接受通用三维云体积、方向环境光和太阳透射率纹理；应用负责把天气模型输出映射成体积、把天空目标作为背景，并做显示变换。

## 范围

- 在屏幕空间沿相机射线积分三维云水场；
- 单次散射（太阳 NEE）、多次散射 octave 近似、天空环境光；
- 与 `sky-realtime` 的屏幕天空合成；
- 不做 3D 路径追踪参考（由 `cloud-pt` 和 `cloud-audit` 负责），不拥有天气模拟，不做显示色调映射。

## 坐标与输入

- 世界单位米，`y` 为海拔；相机 `origin_m` 的 `y` 是观察者海拔。
- 相机射线与 `sky-realtime` 相同：
  `forward=(sin(yaw)cos(pitch), sin(pitch), cos(yaw)cos(pitch))`，
  `right=(cos(yaw),0,−sin(yaw))`，`up=cross(forward,right)`。
- 体积 `dims=[nx,ny,nz]`（x, y 垂直, z），`origin_m`、`spacing_m`；通道 `[LWC kg/m³, r_eff m, ice_fraction, cloud_fraction]`，x 最快。
- 背景为屏幕尺寸的线性 Rec.2020 天空纹理；环境为全向线性 Rec.2020（不含太阳盘）；太阳透射率为 1×1 四波段纹理，附 3×4 权重换算到 RGB。

## 公开 API（v1）

```rust
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality { Balanced, High }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub quality: Quality,
    pub max_steps: u32,
    /// 体积追踪分辨率相对输出的比例 0.25..=1.0；工作台平衡 0.8、高质量 0.85。
    pub trace_scale: f32,
    /// 太阳阴影 march 步数 1..=12；工作台平衡 6、高质量 10。
    pub sun_steps: u32,
    pub ms_octaves: u32,
    /// Stratified ray samples per pixel (1..=16). 8/16 are the app defaults.
    pub samples: u32,
    pub volume_dims: [u32; 3],
    pub extinction_scale: f32,
    pub ambient_scale: f32,
    /// 子网格 3D 细节侵蚀强度 0..1；库默认 0，工作台 0.8/0.9。
    pub detail_strength: f32,
    /// 空气透视 e-folding 距离（米）；0 关闭。工作台 12 km。
    pub aerial_distance_m: f32,
    /// 地面云影强度 0..1；0 关闭。工作台 0.9。
    pub cloud_shadow_strength: f32,
}
impl Default for Config { /* Balanced, 40 steps, 4 sun steps, 2 samples, 3 octaves */ }
impl Config { pub fn validate(&self) -> Result<()>; }

#[derive(Clone, Copy, Debug)]
pub struct VolumeDescriptor {
    pub dims: [u32; 3],
    pub origin_m: [f32; 3],
    pub spacing_m: [f32; 3],
}

#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub origin_m: [f32; 3],
    pub yaw_deg: f32,
    pub pitch_deg: f32,
    pub fov_y_deg: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Sun { pub direction: [f32; 3] }

pub struct Renderer { /* … */ }
impl Renderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, config: Config) -> Result<Self>;
    pub fn resize(&mut self, device: &wgpu::Device, size: [u32; 2]) -> bool;
    pub fn upload_volume(
        &mut self, queue: &wgpu::Queue, data: &[f32], descriptor: VolumeDescriptor,
    ) -> Result<()>;
    /// 上传已按 f16 位模式编码的体积（见 [`encode_volume_half`]）；校验与 GPU
    /// 写入同 `upload_volume`，只跳过 f32→f16 转换，供后台线程预编码后使用。
    pub fn upload_volume_half(
        &mut self, queue: &wgpu::Queue, data: &[u16], descriptor: VolumeDescriptor,
    ) -> Result<()>;
    /// 自渲染器创建以来是否已上传过体积（空体积纹理仍是合法的全零云场）。
    pub fn has_volume(&self) -> bool;
    /// 绑定环境光、1×1 太阳透射率和换到 RGB 的 3×4 权重。
    pub fn set_environment(
        &mut self, device: &wgpu::Device, environment: &wgpu::TextureView,
        sun_transmittance: &wgpu::TextureView, solar_weights: [[f32; 4]; 3],
    );
    pub fn set_background(&mut self, device: &wgpu::Device, background: &wgpu::TextureView);
    /// 子网格细节噪声的世界空间漂移（米，xz）；应用按天气风推进它，使细节随云移动。
    pub fn set_detail_drift(&mut self, drift_m: [f32; 2]);
    /// 基础体积的视觉漂移（米，xz）：两次天气体积上传之间按帧平移 3D 纹理采样
    /// 位置，使云的运动逐帧连续（上传新快照后归零）。非有限值按 0 处理。
    pub fn set_volume_drift(&mut self, drift_m: [f32; 2]);
    /// 设置天气驱动的阴天遮挡系数（0..1，非有限值按 0 处理）。composite pass
    /// 用它压暗天空/地面背景与云辐射；主 pass 同时用它淡出直射太阳和环境光。
    pub fn set_overcast(&mut self, overcast: f32);
    /// 设置对流风暴强度（0..1，非有限值按 0 处理）。只在强对流（闪电/冰雹/
    /// 特大暴雨）时给 composite 额外的近暗压降；普通阴天/降雨不触发。
    pub fn set_storm_severity(&mut self, severity: f32);
    /// 丢弃时间累积历史；天气场被替换后调用，避免旧云场残影。
    pub fn reset_history(&mut self);
    pub fn render(
        &mut self, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder,
        camera: Camera, sun: Sun,
    ) -> Result<()>;
    pub fn target_view(&self) -> &wgpu::TextureView;
    /// Output texture for headless readback (cloud-audit).
    pub fn target_texture(&self) -> &wgpu::Texture;
    pub fn stats(&self) -> &Stats;
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub frames: u64,
    pub volume_uploads: u64,
    pub last_steps: u32,
    pub last_output: [u32; 2],
    /// 体积追踪分辨率；由 `trace_scale` 决定。
    pub last_trace: [u32; 2],
}

/// 把体积载荷从 f32 编码成 f16 位模式（`upload_volume` 内部同样使用它）。
pub fn encode_volume_half(data: &[f32]) -> Vec<u16>;
```

- `resize` 在尺寸变化时重建输出、云缓冲与绑定；返回是否变化。`trace_scale < 1` 时体积 raymarch 在**缩放后的追踪缓冲**（如 1600×900、0.8→1280×720、0.85→1360×765）执行，resolve pass 做时间累积，composite 再以 5×5 双边滤波上采样到全分辨率；工作台平衡档 0.8、高质量档 0.85。`detail_strength` 默认 0，子网格细节路径见下文「渲染实现」。
- 每帧顺序为：体积 pass（每帧抖动）→ temporal resolve（重投影 + 邻域钳制 + 混合历史）→ 把 resolved 拷回 history → composite。`reset_history` 在 resize、相机位移/大幅旋转/FOV 跳变、太阳或 overcast/storm 跳变、天气模型重建时由渲染器/应用触发。
- `upload_volume` 只接受匹配 `Config.volume_dims` 的 `dims`，数据长度 `nx*ny*nz*4`，全部有限非负；内部转 `Rgba16Float`（`half`）。`upload_volume_half` 接受已编码的 f16 位模式载荷，只做分支式位模式校验（6.3M 值约 0.5 ms）和 256 B 对齐时的零拷贝写入；工作台的后台线程做体积重建 + f16 编码，渲染线程只剩校验 + 上传（约 1.4–2.0 ms，见[动态天气与云场](weather.md#模拟速度与帧率)）。
- `set_environment`/`set_background` 在纹理视图变化时重建绑定；未设置时使用 1×1 默认纹理。
- `set_overcast` 是应用层耦合：应用从天气诊断（云量、液水路径）得出 0..1 遮挡系数后显式传入。它不读取天气模型，也不改变消光/相函数；只是一个**有界的阴天光照近似**，用于弥补缺少多次散射/输运云影和漫射照明场。
- `render` 前必须已上传体积（异步管线可用 `has_volume()` 判断并在首帧同步引导）；`overcast = 0` 时零密度体积必须输出背景（逐像素误差 <1e-5）；`overcast > 0` 时背景会被有意压暗（见下文合成）。
- `target_view` 为线性 Rec.2020，应用 present pass 做曝光/色调映射。

## 光学与输运

- 水云：`σ_ext=3·LWC/(2ρ_w·r_eff)`；`ω0≈0.999`，`g=clamp(0.85−0.02·log2(r_eff/10µm), 0.78, 0.88)`；相函数为双叶 HG（0.75/0.25）。**v1 没有 Mie/Hu–Stamnes 表**，可见光灰体近似；卷云用较低 g、较大有效半径的等效近似，不渲染晕。
- 太阳 NEE：沿太阳方向积分光学厚度；步数由 `Config.sun_steps`（1..=12）控制，工作台平衡档 6 步、高质量档 10 步；直射辐照度由 `solar_weights` × 1×1 透射率在 shader 内得到。`overcast` 越接近 1，直射太阳项与天空环境光按固定曲线淡出（云层遮蔽太阳的近似）；NEE 对厚阴天云本身已经饱和，但对薄云仍是粗估计，且不采样 shadow map（地面云影由独立 512² map 提供）。NEE 只采样**基础体积密度**（不叠子网格细节），避免光照路径重复采样 3D 细节纹理；主视线仍按需叠细节。
- **Beer-Powder**：`powder = 1 - e^{-2σ_t·350 m}`，对直射项乘 `mix(0.62, 1.0, powder)`，压暗光学薄边缘，让积云呈“糖堆”体积感。长度尺度是**视觉调参**，不是能量守恒修正。
- 多次散射：3 个固定 octave（消光 ×0.5^k、权重 0.5^k、g^(k+1)），总和再乘 0.3/0.7 的权；**未完成对 MC/DISORT 网格的拟合**，v1 只做非负/能量量级测试。
- 环境光：从全向环境纹理按视线方向取一个方向（非余弦积分）；v1 的误差未量化。阴天时环境光向中性灰混色（近似平谱阴天光），避免云底偏蓝。
- 合成：`L = s_cloud·L_cloud + T_cloud·(s_sky·L_sky)`；`s_sky`/`s_cloud` 先按 `overcast` 插值（`s_sky: 1→0.26`、`s_cloud: 1→0.42`），再乘风暴强度因子（severity 0..1 时 `s_sky ×(1→0.30)`、`s_cloud ×(1→0.34)`）；severity 超过 1 的部分（深对流/超级单体，最多 1.5）再乘一档近暗因子（`s_sky ×(1→0.50)`、`s_cloud ×(1→0.40)`）。背景同时向中性灰混色，并按地面云影 map 压暗。由独立 composite pass 完成；composite 先把（可能低分辨率的）云缓冲用 5×5 双边上采样到全分辨率，再与全分辨率背景合成。v1 不含相机到云前的物理空气透视（只有视觉混合近似）、云-天空双向反馈；阴天/风暴压暗是**有界近似**，不是辐射输运结果。
- 云亮度在 v1 乘 0.55 的经验系数；多次散射 octave 权重未拟合前，云顶容易过亮，这是显式妥协。
- 厚阴天结构：厚云在 ambient 主导下会收敛成平灰一面墙；`overcast`/风暴强度较大时，云光照额外叠加低频三角函数结构调制与云底更暗的高度梯度。它是**风格化细节项**，不改变消光或多次散射物理，也不是输运的密度/阴影场。

## 渲染实现

- WGSL 位于本 crate（`clouds.wgsl` + `resolve.wgsl` + `composite.wgsl`），通过 naga 解析/验证和 SPIR-V 生成测试。
- 体积 ray–box 相交；每个**追踪像素** 1–16 个分层样本（平衡档 12、高质量档 16）；步长按 `u^3` 向相机集中；每步抖动带帧序号，使时间累积不断获得新样本。
- **体积运动插值**：天气体积只以约 1–10 Hz 更新，主 pass 的 `cloud_sample` 在采样 3D 纹理时加上 `Renderer::set_volume_drift` 的偏移（米，xz）。偏移 = 风矢量 × 上次体积快照以来经过的连续模拟秒数，上传新快照时归零；它把两次重建之间的风平流逐帧补齐，避免高模拟速度下云按重建频率“停格”。偏移只影响采样（不重采样数据），地面云影 map 不叠加该漂移（误差小于一个体素）。
- 主 pass 输出 `radiance + transmittance` 到追踪缓冲（分辨率为输出 × `trace_scale`）。composite pass 用 5×5 双边滤波（transmittance + 亮度引导）上采样，再与全分辨率背景合成；双边核在云边缘不会把透射率梯度抹平。
- **轻量 TAA**（`resolve.wgsl`）：workbench 相机只旋转不平移，因此用当前世界方向旋回上一帧相机基做重投影；历史先钳制到当前 3×3 邻域 min/max，再以 `alpha` 混合（云移动少时 0.08，随时间步长/漂移升高到 0.62）。相机位移、FOV 跳变、太阳/overcast/storm 跳变和天气模型重建都会丢弃历史。没有深度、运动矢量或视差修正，所以快速平移和高倍率时移会有界残影。
- **空气透视（近似）**：raymarch 主循环累计“辐射加权云距离”，然后按 `1 - exp(-d/aerial_distance_m)` 把云辐射混向该视线方向的天空环境色。它补偿“云在真空里积分、背景天空已含大气”的不一致，让远处云层自然融入天空；**不是**光谱透射率 LUT 或逐段大气积分。
- **地面云影**（`shadow.wgsl`）：体积上传或太阳方向变化时，用 512² 的 top-down shadow map 沿太阳方向积分云光学厚度，存 `exp(-tau)`；composite pass 从相机射线求 y=0 地面命中点，用 **4-tap PCF + 双线性**采样，乘到背景上。阴影强度由 `Config.cloud_shadow_strength` 控制，暗化上限 55%（天空光仍照亮地面），并只在体积覆盖域内生效。它是单次散射阴影，不是多次散射/云间互反射；不改变云自身亮度，也不做天空中的云影。
- v1 仍没有深度引导/视差上采样或 TAA 深度/运动矢量。
- 不写深度；不依赖 `sky-realtime` 内部类型。
- 任何非有限/负值/越界输入返回错误，不静默产生黑像素。

### 子网格细节侵蚀（工作台质量档默认开启）

天气体积为 128×96×128 / 32×14×32 km（水平 250 m、垂直 146 m 体素，由 `sky-weather` 按 z 切片并行重建）。250 m 体素线性过滤后云界呈单个体素级的阶梯，厚云层内部也缺少起伏。`sky-clouds` 内置一张 64³、三通道、可平铺的 3D 细节噪声（R/G/B 分别为 4/16/32 基频的 value-noise FBM，生成时按通道做对比度归一化），shader 按**四级距离 LOD** 从中取 2–4 次采样：

| 尺度 | 特征大小 | 距离 LOD |
|---|---|---|
| R 通道，8192 m 采样尺度 | 约 0.25–2 km | 全程 1.0 |
| R 通道，4096 m 采样尺度 | 约 128–1024 m | 25–36 km 淡出 |
| G 通道，4096 m 采样尺度 | 约 128–256 m | 3.0–6.5 km 淡出 |
| G 通道，1024 m 采样尺度 | 约 32–64 m | 0.9–2.6 km 淡出 |

宽/中尺度负责把远处 250 m 体素边界打散成不规则轮廓，细/微尺度负责近云的花椰菜状细节；更远的高频会低于两个 trace 像素、产生闪烁，因此按距离淡出。细/微尺度的合成权重分别压到 0.30/0.12（而宽/中为 1.0/1.0），让轮廓由 0.3–1 km 的花椰菜结构主导，而不是 50–150 m 的“碎纸边”。`Config.detail_strength`（0..1，库默认 0，工作台平衡 0.8 / 高质量 0.9）控制侵蚀强度。

细节以"成员关系变形"使用：多尺度噪声和重映射成 `mix(0.15, 2.0, smoothstep(0.30, 0.70, n))` 的密度乘子，约一半样本被压到基础密度以下，让 τ≈1 的等值面沿噪声局部移动。乘子只作用于光学薄样本（`1 - smoothstep(0.05, 0.30, σ_base)`），厚云内部保持相干，避免体积积分出现斑点/闪烁；当乘子为 0（厚云内部或 detail 关闭）时 shader 直接返回 1.0，跳过纹理读取。太阳 NEE 仍只采样基础体积密度，保持光路便宜且稳定。

本轮另加两个廉价体积感项：

- **天空遮挡近似**：每个着色样本向 +550 m 采一次基础密度，用 `exp(-tau_up)` 衰减环境光（下限 0.32），云底/云下侧比云顶暗；
- **高度基础变暗**：`mix(0.85, 1.0, smoothstep(0, 0.32, h))` 对所有云类型生效。

证据（2026-10-18）：同树 A/B（旧 shader vs 新 shader，同体积/相机/太阳）在 1600×900 捕获中渲染成本基本不变（fair 4.72→4.59 ms、partly 5.65→5.51 ms，约 −2.5%），新增的天空遮挡采样被"厚云跳过细节读取"抵消；partly 云层均值亮度 0.766→0.689、对比度标准差 +9%、高频能量基本不变（无闪烁）。云缘从光滑团块变为更碎的花椰菜状。

已知限制：对**光学厚、且只被 1–2 个步长穿过的远云**（fair 云在 8–10 km 处 march 步长约 1.3 km，云本身约 0.7 km 宽），逐样本密度乘子会被逐帧抖动与 TAA 平均回均值，τ≈1 的轮廓几乎不动，250 m 体素阶梯仍可见。彻底解决需要密度引导的射线分段或更高体积分辨率，而不是继续加强 shader 密度调制。

## 测试与已知差距

已实现：

- naga 解析 + 验证 + SPIR-V；相机基正交；体积上传尺寸/有限性拒绝；`upload_volume_half` 的位模式校验（负值/非有限值/长度/几何）与 `encode_volume_half` 往返一致；解析 slab 光学厚度；零密度 CPU 复合；`composite_with_overcast` 的阴天 + 风暴压暗/中性化，以及 `set_overcast`/`set_storm_severity` 的钳制规则；细节漂移的有限性/归一化、追踪分辨率缩放、时间混合 alpha 边界、空气透视距离/云影强度校验（`sky-clouds` 现 23 个测试）。工作台异步体积管线（工作线程、代际失效、模型快照逐位一致）与连续云运动（漂移插值、自适应重建间隔）另有 5 个测试覆盖（`sky-demo` 4、`sky-weather` 1）。
- 实际 GPU `cloud-audit render-cloud --zero-density`：输出逐像素等于背景，`zero_density_max_abs_error ≈ 6e-8`（160×90，Rgba32Float 读回，低于 1e-5 门槛；cloud-audit 未设置 overcast，因此是 `overcast = 0` 的恒等式）。
- `cloud-audit render-cloud --zero-density --trace-scale 0.5`：追踪缓冲为 80×45（输出 160×90），恒等式误差同为 `5.96e-8`，证明双边上采样 + temporal resolve 对零密度不引入偏置；JSON 记录 `trace` 与 `trace_scale`。
- 实际 GPU 云图：`cloud-audit render-cloud --kind rain` 能生成云/背景合成，JSON 记录均值与调参。
- 工作台端统计对比（`out/qa2/`）：同一相机/太阳下 clear / overcast / rain / thunderstorm 的天空与地面亮度随遮挡系数下降；见 [动态天气与云场](weather.md#本机验证)。

未完成（验收差距）：

- 与 `cloud-pt` MC 的 slab/3D 图像 P95 对照；
- MS octave 的 MC 拟合与 DISORT/两流对照；
- 1080p 性能测量（当前只有 1600×900 工作台捕获）；
- TAA 只用旋转重投影，没有深度/运动矢量/视差修正：相机平移、快速旋转和高倍率时间加速时只能靠邻域钳制**有界**残影，不是消除；
- 追踪分辨率（`trace_scale`）+ 双边升采样能显著降本，但云缘仍比全分辨率档更“颗粒”，且没有深度引导；
- 体积 250 m 体素 + 线性过滤仍是云边界模糊的主因；本轮的四级细节 LOD 显著改善了近云与云层结构，但**远距光学厚云的轮廓仍受 march 步长限制**（8–10 km 处步长约 1.3 km，逐样本细节调制会被抖动/TAA 平均掉）。下一步应做密度引导的射线分段或更高体积分辨率/自适应域，而不是继续加强密度乘子；体积重建已并行化（最多 8 线程，约 13.6 ms/1.57M 体素）并已异步化；
- 云影仍是**单次散射 top-down shadow map**（仅地面，暗化上限 55%），没有多次散射/云间互反射，也不把云影投到天空；空气透视是**视觉混合近似**（辐射加权距离 → 混向天空色），不是光谱透射率 LUT，也没有云-天空双向反馈；阴天压暗仍是应用层近似；
- **解析步长积分已试过但回退**：`(1-e^-σdt)/σ` 会正确去掉步长偏差，但把厚云压暗约 15–40%（fair 白云 219→182、雷暴天空 20.9→11.5/255）。本轮再次尝试用提高 MS octave 系数（1.5→2.0）和全局系数（0.70→0.80）来补偿：fair 白云仍只有 **190**、partly **175**、雷暴天空 **17–18**（基线 214/197/22.6），云整体明显发灰，因此回退。结论：现有 octave 结构需要先对 MC/DISORT 拟合出正确的 `a_k/b_k/g_k`，不能用两个全局系数硬凑亮度。

性能目标（待测量，不作为已实现保证）：1080p 平衡档 ≤1.2 ms、高质量 ≤2.5 ms（RTX 4090 参考，记录适配器/分辨率/步数/样本数）。

### 本轮实测（2026-10-18 迭代 20，1600×900 / 1080p 未限帧捕获）

`sky-weather-workbench --smoke 30 --width 1600 --height 900 --warmup-seconds 90`，固定天气、手动太阳 47.9°/105.4°。平衡档：32 步 / 12 样本 / 6 太阳步 / `trace_scale=0.8` / 细节 0.8；高质量档：40 步 / 16 样本 / 10 太阳步 / `trace_scale=0.85` / 细节 0.9。两档均含 TAA、5×5 双边升采样、Beer-Powder、12 km 空气透视、512² 地面云影、128×96×128 体积。同机重复测量波动约 ±5–10%，个别受后台负载污染的运行整体偏高 30%+（已剔除）；下表为同一轮干净捕获，1080p 行使用 `--width 1920 --height 1080`：

| 场景 | 中位 | 均值 | P95 | 最大 | 备注 |
|---|---:|---:|---:|---:|---|
| fair（平衡） | 4.63 ms | 5.87 ms | 8.51 ms | 12.11 ms | 0.8 trace / 12 样本 / 32 步 |
| partly_cloudy（平衡） | 7.39 ms | 7.72 ms | 10.23 ms | 13.49 ms | |
| partly_cloudy（平衡，pitch 32°） | 8.40 ms | 8.38 ms | 9.97 ms | 12.97 ms | 仰视云层结构 |
| thunderstorm（平衡） | 8.71 ms | 9.32 ms | 11.87 ms | 14.59 ms | 含雨/闪电效果 |
| fog（平衡） | 10.55 ms | 9.90 ms | 12.07 ms | 15.46 ms | |
| fair（高质量） | 8.02 ms | 8.08 ms | 10.99 ms | 13.32 ms | 0.85 trace / 16 样本 / 40 步 |
| partly_cloudy（高质量） | 13.06 ms | 13.16 ms | 15.17 ms | 17.18 ms | 高质量档最重场景 |
| fair（平衡，1080p） | 8.16 ms | 7.92 ms | 9.75 ms | 13.02 ms | 1920×1080 |
| partly_cloudy（平衡，1080p） | 12.08 ms | 11.76 ms | 14.33 ms | 16.96 ms | 1920×1080 |
| partly_cloudy（高质量，1080p） | 20.80 ms | 20.79 ms | 22.98 ms | 24.56 ms | 1920×1080 |

- **质量档调整（本轮重点）**：旧平衡档 0.65× 追踪（1040×585）+ 8 样本 + 24 步时，云缘有明显 per-trace-pixel 阶梯（放大看像小方块），与 250 m 体素一起造成“Minecraft 云”观感。本轮把平衡档提高到 0.8×（1280×720）+ 12 样本 + 32 步，高质量档到 0.85×（1360×765）；放大对比（`out/iteration-20/crops/quality-grid-v2.png`，旧平衡 / 新平衡 / 旧高质量 / 新高质量）中阶梯与斑点明显减少、云体更连贯。代价是显式的：平衡档 1600×900 成本从 fair 2.5 / partly 3.5–4.1 ms 升到 4.6 / 7.4 ms，1080p 平衡 partly 12.1 ms（≈83 fps）；高质量 1080p partly 20.8 ms（≈48 fps）。这是“云缘更干净 vs 更多像素成本”的取舍。
- **细节侵蚀调参**：细/微尺度合成权重 0.55/0.30 → 0.30/0.12，淡出范围收紧到 3.0–6.5 km / 0.9–2.6 km，宽/中尺度保持到 25–36 km。云缘从“碎纸边 + 斑点”变为更连贯的 0.3–1 km 花椰菜轮廓。
- **speed=1 捕获**：partly 90 帧中位 6.07、P95 6.48、最大 6.91 ms（新质量档）；30 帧内云基本静止、TAA 完全收敛，用于检查细节无闪烁。
- **speed=60 体积更新**：120 帧捕获中位 6.74、P95 9.77、最大 12.80 ms。绝对值随新质量档上升，但重建尖峰仍被消除（旧同步实现同场景最大 49.12–53.69 ms）；两次重建之间的云运动由体积漂移插值逐帧补齐（重建间隔随模拟速度自适应收紧到 0.10–0.28 s）。
- **体积重建与上传**：128×96×128（1.57M 体素）约 13.6 ms/次（8 线程并行），重建与 f16 编码都在后台工作线程；渲染线程只剩校验 + `write_texture`，实测上传帧约 1.4–2.0 ms。异步前同一帧为 14–17 ms，其中 f16 校验 10–13 ms、行重排 3.0–3.9 ms，现已分别改为分支式位模式校验和 256 B 对齐零拷贝。
- **云影**：512² top-down shadow map 只在体积上传或太阳方向变化超过约 0.57° 时重算；composite 从相机射线求地面命中点，用 4-tap PCF + 双线性采样。暗化上限 55%，避免出现纯黑洞；雷暴地面亮度 69.2→50.7（27% 压暗），天空不变。PCF 约增加 0.1–0.2 ms；阴影仍偏软/低分辨率，且只作用在 y=0 平面上。
- **云形**：四级细节 LOD + 仅薄样本侵蚀 + 天空遮挡 + 云底变暗；fair 云缘从阶梯状变为不规则花椰菜状，partly 低层云有清晰亮暗起伏与更暗的云底。远处厚云轮廓仍受 250 m 体素与步长限制（见“状态与后续”）。
- **追踪分辨率缩放**：`trace_scale` 仍是单旋钮（平衡 0.8→1280×720，高质量 0.85→1360×765）；`cloud-audit render-cloud --trace-scale <0.25..1> --detail-strength <0..1> --max-steps/--samples/--sun-steps` 可复现单帧对照。
- **空气透视**：辐射加权云距离按 `1 - exp(-d/12 km)` 把云色混向视线方向天空色；远处云层融入天空。视觉近似，不是逐段大气透射率积分。
- **太阳阴影步数**：`Config.sun_steps` 1..=12；工作台平衡 6、高质量 10。
- 捕获路径不是交互帧率；绝对帧率取决于 GPU/负载，同机重复测量差异在此类长对话中明显大于上一轮记录的 ±3%。

## 状态与后续（2026-10-18 迭代 20）

- **体积**：128×96×128 / 32×14×32 km，按 z 切片并行重建（≤8 线程、同 seed 逐位一致、约 13.6 ms/1.57M 体素）；重建与 f16 编码在后台工作线程，渲染线程只做校验 + 上传（异步前 14–17 ms 尖峰 → 约 1.4–2.0 ms）。重建间隔随模拟速度自适应收紧（0.10–0.28 s），两次上传之间由渲染端体积漂移逐帧平移采样位置，保证云的运动连续；体积噪声的平流方向已修正为顺风（与细节噪声和天气风向一致）。噪声 octave 按 250 m/146 m 体素带宽限制（≥约 4 体素/波长，移除了 750/520 m 的欠采样 octave），次体素结构交给渲染端细节侵蚀；覆盖率掩膜、云顶/云底低频起伏、风向高度剪切保留。
- **细节**：64³ 三通道可平铺 3D 细节噪声；四级距离 LOD（0.25–2 km 全距离、128–1024 m 到 25–36 km、128–256 m 到 3.0–6.5 km、32–64 m 到 0.9–2.6 km；细/微合成权重 0.30/0.12）；成员关系变形侵蚀只作用于光学薄样本；天空遮挡 + 云底高度变暗；太阳 NEE 只用基础密度。
- **采样**：`trace_scale` 连续分辨率旋钮（平衡 0.8、高质量 0.85）、每像素 12/16 分层样本、32/40 步、旋转重投影 TAA + 5×5 双边升采样；零密度恒等式 `5.96e-8`。
- **光照**：太阳 NEE（6/10 步，仅基础密度）、双叶 HG、3 个固定 MS octave、天空环境光（0.25/0.24）、消光 1.35、Beer-Powder、`sun_steps` 质量旋钮；解析步长积分已尝试但回退（需 MS 拟合）。
- **耦合**：12 km 空气透视（视觉混合近似）、512² top-down 地面云影（4-tap PCF、暗化上限 55%）、应用层 overcast/storm 压暗。
- **性能**（捕获，非交互帧率）：1600×900 平衡 fair 4.63 / partly 7.39 / partly-up32 8.40 / storm 8.71 / fog 10.55 ms；高质量 fair 8.02 / partly 13.06 ms。1080p 平衡 fair 8.16 / partly 12.08 ms；1080p 高质量 partly 20.80 ms。质量档提高是用成本换云缘干净度的显式取舍。

仍存在的明确差距（也是下一阶段路线）：

1. **远距厚云轮廓仍受步长限制**：fair 云在 8–10 km 处 march 步长约 1.3 km、云本身约 0.7 km 宽，逐样本密度乘子被抖动/TAA 平均回均值，τ≈1 等值面几乎不动，250 m 体素阶梯仍可见。候选方案：密度引导的射线分段（粗分段定位云区后加密步进）或更高体积分辨率/自适应域；继续加强 shader 密度调制作用有限。
2. **MS octave 未对 MC/DISORT 拟合**，云的反照率/多次散射能量没有物理标定；这是启用解析积分的前提。
3. TAA 没有深度/运动矢量；相机平移、快速旋转、高倍率时间加速时只有有界残影。
4. 云影是单次散射、只作用 y=0 地面，没有云影投天、云间互反射或更高质量的 PCF。
5. 空气透视是视觉混合，不是光谱透射率 LUT / 逐段大气积分。
6. 云光学仍是灰体双叶 HG，没有 Mie/Hu–Stamnes 表；卷云/冰晶未单独建模。
7. 目前只有短时离屏捕获测量（1600×900 与 1080p）；仍缺长时交互帧率、不同 GPU、真实照片/`cloud-pt` P95 的验收数据。

## 依赖与所有权

- 不得依赖 `sky-pt`、`sky-reference`、`sky-realtime`、`cloud-pt`、`sky-weather` 或 `sky-assets`。
- 不读取天气模型的数据目录；应用负责体积映射。
- 数据/拟合参数放 `crates/sky-clouds/data/`，附生成工具与校验和；不得读取 `out/`。

## 参考文献（实现依据）

- Hansen, J. E., & Travis, L. D. (1974). Light scattering in planetary atmospheres. *Space Sci. Rev.* 16, 527–610. doi:10.1007/BF00168069
- Wiscombe, W. J. (1980). Improved Mie scattering algorithms. *Appl. Opt.* 19, 1505–1509. doi:10.1364/AO.19.001505
- Hu, Y.-X., & Stamnes, K. (1993). An accurate parameterization of the radiative properties of water clouds suitable for use in climate models. *J. Climate* 6, 728–742.
- Joseph, J. H., Wiscombe, W. J., & Weinman, J. A. (1976). The delta-Eddington approximation for radiative flux transfer. *JAS* 33, 2452–2459.
- Meador, W. E., & Weaver, W. R. (1980). Two-stream approximations to radiative transfer… *JAS* 37, 630–643.
- Stamnes, K., et al. (1988). Numerically stable algorithm for discrete-ordinate-method radiative transfer… *Appl. Opt.* 27, 2502–2509.
- Bouthors, A., et al. (2008). Interactive multiple anisotropic scattering in clouds. *Proc. I3D 2008*, 173–182. doi:10.1145/1342250.1342277
- Wrenninge, M., et al. (2013). Oz: the great and volumetric. *SIGGRAPH 2013 Talks*. doi:10.1145/2504459.2504518
- Hillaire, S. (2020). A scalable and production ready sky and atmosphere rendering technique. *CGF* 39(4), 13–22. doi:10.1111/cgf.14050
- Marshak, A., & Davis, A. B. (eds.) (2005). *3D Radiative Transfer in Cloudy Atmospheres*. Springer.
- Schneider, A., & Vos, N. (2015). The real-time volumetric cloudscapes of Horizon Zero Dawn. *SIGGRAPH 2015 Advances in Real-Time Rendering*. https://www.advances.realtimerendering.com/s2015/
- Schneider, A., & Vos, N. (2017). Nubis: authoring real-time volumetric cloudscapes with the Decima engine. *SIGGRAPH 2017 Advances in Real-Time Rendering*. https://www.realtimerendering.com/advances/s2017/
- Guerrilla Games (2023). Nubis³: real-time volumetric cloudscapes for Horizon Forbidden West: Burning Shores. https://www.guerrilla-games.com/read/nubis-cubed
- Toft, J., Bowles, H., & Zimmermann, J. (2016). Quarter-resolution volumetric clouds. arXiv:1609.05344. https://arxiv.org/abs/1609.05344
- Epic Games. Volumetric Cloud Component and Volumetric Cloud Material documentation (UE5). https://dev.epicgames.com/documentation/en-us/unreal-engine/volumetric-cloud-component-in-unreal-engine
- Unity Technologies. HDRP Volumetric Clouds documentation. https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@12.0/manual/Override-Volumetric-Clouds.html
