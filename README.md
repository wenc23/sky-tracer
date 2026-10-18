# Sky Tracer：大气与云光传输研究

这是一个研究型 Rust / wgpu 项目。目标是在固定大气和地面参数下，渲染任意太阳位置、观察海拔和方向的天空，并为有限视距的雾提供光传输接口。GPU 输运使用 **f32**。

大气部分保留三个独立求解器：路径追踪检查物理输运，完整光谱 LUT 提供低噪声研究数据，四波长实时求解器负责交互显示。参考数据有偏，实时结果也有近似；两者都不能自动当作真值。另有独立的 VDB 云路径追踪器，用于研究体散射和生成 Monte Carlo 参考图像。

## 从哪里开始

|文档|回答的问题|
|---|---|
|[光如何形成天空](docs/atmosphere.md)|散射、光谱、四维状态是什么？三个算法与参考项目有什么关系？|
|[数值精度与数据分配](docs/numerics.md)|为什么出现圆环、地平线和地影误差？怎样拟合坐标、波长并检验结果？|
|[实时方案与压缩取舍](docs/realtime.md)|当前配置、性能、质量边界，以及 BC6H、低秩和球谐的实验结论。|
|[研究与复现指南](docs/research.md)|代码边界、数据格式、实验流程和可运行命令。|
|[云路径追踪参考](docs/clouds.md)|Disney VDB、无偏估计器、稀疏加速、统计输出与运行方式。|
|[动态天气与云场](docs/weather.md)|行为优先的 20 种天气、状态机、二维场、天气系统与边界。|
|[实时天气效果](docs/weather_fx.md)|屏幕空间雨/雪/雾/沙尘/雷电/彩虹、零效果恒等式与合成。|
|[实时云输运](docs/clouds_realtime.md)|云光学、多次散射、raymarch、与天空合成及验证。|
|[把天空搬进 Minecraft](docs/minecraft.md)|光影包烘焙了什么、GLSL 与求解器如何对应、离线与实机验证结果和边界。|

不熟悉图形学时，先读第一篇，再运行 demo。其余文档按关注点查阅。

## 代码布局

天气/云是行为优先实验功能：快速二维场 + 20 种天气状态 + 实时云输运 + 屏幕空间雨雪雾雷电。它**不是**物理天气模拟、3D LES 或预报；边界见 [动态天气与云场](docs/weather.md) 与 [实时天气效果](docs/weather_fx.md)。

天气/云可视化工作台是独立入口 `apps/sky-weather-workbench`（`cargo run --release -p sky-weather-workbench`，或 `.\run.cmd workbench`，窗口标题「天气可视化工作台」）：它复用 `sky-demo` 的显示外壳，界面为中文，在窗口右侧「设置面板」配置 20 种天气、模式/气候/强度/速度/seed、季节与站点太阳时间、自动太阳、云与效果质量、**云层遮光强度**、太阳与观察者、相机和显示；实时画面按 `sky-realtime` 天空 → `sky-clouds` 体积云 → `sky-weather-fx` 屏幕空间效果 → 显示变换的顺序合成。阴天/雷暴会按云量与含水量压暗天空、地面与云底，并淡出太阳盘；强对流（雷暴/冰雹/超级单体）再叠一层近暗压降，做出「黑云压城」的观感，普通阴天保持白昼灰。这是应用层光照近似，不是输运云影；它是行为优先的可视化工作台，不是天气数据查看器或预报工具。体积云重建在后台工作线程完成（异步，不阻塞渲染），云形细节由渲染端四级 detail 噪声给出；选项、HUD、冒烟截图与本机实测（1600×900 平衡档约 4–11 ms/帧、1080p 约 8–21 ms/帧，未限帧捕获）见 [动态天气与云场](docs/weather.md) 与 [实时云输运](docs/clouds_realtime.md)。

```text
crates/
  sky-pt/          独立光谱路径追踪器；自有物理表、采样器、GPU 输运
  sky-reference/   独立光谱参考 LUT；烘焙、查询、CPU 数据合成、资源编码
  sky-realtime/    独立四波长实时算法；启动求解、SkyView、有限段 L/T
  sky-weather/     独立行为天气核心；20 种状态、director、二维场、云体积
  sky-weather-fx/  独立屏幕空间天气效果；雨雪雾沙尘雷电彩虹
  sky-clouds/      独立实时云输运；Mie/光学近似、多次散射、raymarch 合成
  sky-assets/      应用之间的图像清单格式；不含介质或输运
  cloud-pt/        独立 VDB 云路径追踪器；稀疏密度、局部 majorant、CPU/GPU 输运
apps/
  sky-baker/       PT 图像与参考资源的命令行烘焙入口
  sky-demo/        实时显示、参考 LUT 显示、PT 对照、天气/云实验和性能测量
  sky-weather-workbench/ 天气/云实时可视化工作台入口；交互 + 冒烟截图
  sky-audit/       固定场景评估、缓存与输运恒等式检查
  sky-optimizer/   Python / CPU 波长搜索、候选比较和图集
  cloud-demo/      全帧云 PT + 实时天空、参数 UI/移动相机、离线 EXR 与资产分析
  cloud-audit/     天气/云核心的验证、参考对照和性能基准
  sky-environment/ 导出冻结的环境光照资产（全向辐亮度 + 太阳信息，供外部渲染器消费）
  sky-pack/        把实时求解结果烘焙成 Minecraft 光影包，并做离线 GLSL/输运验证
mc/
  SkyTracer/       Iris/OptiFine 光影包源；纹理表与常量由 sky-pack 生成
experiments/
  validation/     已纳入版本管理的相机与掠角回归场景
scripts/          架构检查与通用二进制图像对比
```

七个求解核心 crate **互不依赖，也不读取彼此的数据目录**。大气核心各自拥有物理表、色度数据和参数；天气核心拥有行为目录/场，效果核心拥有屏幕空间合成，云核心拥有 VDB/体积输运。对比由应用显式组织。应用只负责参数、I/O、展示和实验统计，不实现介质散射积分、天气行为或效果合成。

## 快速运行

需要支持 Rust 2024 edition 的工具链，以及 wgpu 可用的显卡。已验证环境是 Windows、Vulkan（RTX 4090；天气/工作台路径另在本机 AMD Radeon 8060S 核显上实测）；尚未声明其他 GPU 的性能或一致性。

在仓库根目录执行：

```powershell
# Windows 统一入口（中文交互菜单；自动选择 pwsh / Windows PowerShell）。
# .\run.cmd help 列出全部命令与别名；其余命令也可以直接 cargo run。
.\run.cmd
.\run.cmd workbench    # 天气/云实时可视化工作台（默认自动天气）
.\run.cmd smoke        # 冒烟截图：离屏渲染后写 PNG 并自动退出

cargo build --release --workspace
# 天气/云实时可视化工作台（新入口；中文界面，配置在窗口右侧「设置面板」，默认自动天气）。
cargo run --release -p sky-weather-workbench
# 固定天气（20 种之一；--kind 在 auto 模式下隐含 fixed），例如雷暴/大雪。
cargo run --release -p sky-weather-workbench -- --mode fixed --kind thunderstorm
# 冒烟截图：无人工操作，离屏走同一管线 + UI，写 PNG 后自动退出。
cargo run --release -p sky-weather-workbench -- --smoke 24 --warmup-seconds 120 --screenshot out/workbench-weather-smoke.png --width 1600 --height 900
# 无需先烘焙资源，默认使用新的实时算法。
cargo run --release -p sky-demo
# 行为优先天气实验（自动演变 + 云/雨雪雾雷电效果）。
cargo run --release -p sky-demo -- --experiment weather
# 无窗口天气快照（不要求 PT 参考资产）。
cargo run --release -p sky-demo -- --experiment weather --snapshot out/weather.png --snapshot-pitch-deg 20
# 固定天气/气候的天气快照。
cargo run --release -p sky-demo -- --experiment weather --snapshot out/weather-rain.png --snapshot-weather-mode fixed --snapshot-weather-kind rain --snapshot-weather-spinup-s 120
# 20 种天气目录与自动演变审计。
cargo run --release -p cloud-audit -- weather-catalog --out out/weather-catalog --seconds 120
cargo run --release -p cloud-audit -- weather-simulate --climate temperate --hours 6 --out out/weather-auto
cargo run --release -p cloud-audit -- benchmark --out out/weather-benchmark
# 无窗口 GPU 云图与零密度恒等式检查。
cargo run --release -p cloud-audit -- render-cloud --kind rain --out out/cloud-preview
cargo run --release -p cloud-audit -- render-cloud --zero-density --out out/cloud-zero --width 160 --height 90
# GPU 天气效果零输入恒等式（背景逐像素复制）。
cargo run --release -p cloud-audit -- effects-zero --out out/effects-zero --size 64
# 可选：加载已有 PT 图像作对照。
cargo run --release -p sky-demo -- --asset out/pt_noon_085/asset.json
# 很小的 PT 图像，用于检查管线，不是质量参照。
cargo run --release -p sky-baker -- pt --width 32 --height 16 --spp 64 --out out/pt-smoke
# 只检查完整参考的资源预算，不启动烘焙。
cargo run --release -p sky-baker -- reference plan
# 导出冻结环境光照资产（全向线性 sRGB 辐亮度 + 太阳辐照度，供外部渲染器消费）。
cargo run --release -p sky-environment -- --out out/env_noon --sun-elevation-deg 35 --sun-azimuth-deg 30
# 把实时求解结果烘焙成 Minecraft 光影包（写到 out/shaderpacks/SkyTracer 与同名 zip）。
cargo run --release -p sky-pack -- bake
# 不启动游戏，用本机 OpenGL 编译包内 GLSL，并与逐像素输运对照。
cargo run --release -p sky-pack -- preview
```

完整教师 LUT 使用 safetensors，按含容器头和元数据预留的上界约 **19.001 GB**，新烘焙硬上限为 **20 GB（20,000,000,000 字节）**。它不是首次运行的前提。实时算法不加载这份资源，常驻计算载荷约 **7.910 MiB**。已有二进制资源继续可读；使用参考显示时显式选择：

```powershell
cargo run --release -p sky-demo -- --experiment reference --lut out/lut_reference_v6_packed16
```

每个命令支持 `--help`。数据产物写入忽略跟踪的 `out/`，不要把个人机器的输出路径作为算法的运行依赖。旧实时实现和一次性实验已从当前树移除，需要考古时可查阅 Git 历史中的 `4c02d7c`；已有输出资源未删除。
