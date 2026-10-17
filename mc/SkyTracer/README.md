# Sky Tracer — 四波长物理天空光影包

把 [sky-tracer](../../README.md) 研究项目里的实时大气求解器搬进 Minecraft 的光影包。
固定介质（光学厚度、方向性多重散射源）离线烘焙；太阳连续移动的部分由
`deferred` pass 每帧重建 256×256 的全天球 SkyView，最终由 `composite` pass
替换天空并绘制物理太阳圆盘。地形/水使用最小的 `gbuffers_terrain`/`gbuffers_water`
程序，保留原版贴图、lightmap 光照与雾。

- 物理模型与求解器：`crates/sky-realtime`（同一份 GPU 求解结果）
- 重投影算法：四波长输运 → Rec.2020 → 项目自己的 Enhanced Reinhard 显示变换
- 太阳圆盘默认按物理角径的 4 倍绘制（真实太阳只有 0.53°，在 MC 里太小）；包设置里的 `SKY_SUN_SIZE` 可调
- 已实机验证：MC 1.20.1 + Fabric + Iris 1.7.6 + Sodium 0.5.13（854×480 下约 73 fps）

## 安装

需要 Iris（推荐 1.7+，Minecraft 1.20.1 / 1.21.x）。OptiFine 未测试。

把整个 `SkyTracer` 文件夹（或 `out/shaderpacks/SkyTracer.zip`）放进 `.minecraft/shaderpacks/`，在光影选择界面启用。

如果之前装过旧版本，请注意两个实机修复：`colortex5Format` 必须单独一行写在块注释里，
并且包里必须带 `gbuffers_terrain`（否则 Sodium 会跳过区块）。

## 重新烘焙

在仓库根目录：

```powershell
cargo run --release -p sky-pack -- bake
```

会生成 `out/shaderpacks/SkyTracer/`。`preview` 子命令用本机 OpenGL 离屏编译
并渲染同一批 GLSL 文件，与 `sky-realtime` 的逐像素输运对照，不需要安装 Minecraft：

```powershell
cargo run --release -p sky-pack -- preview
```

## 已知边界

- 不含云、月亮、星空和气辉；夜间天空按物理模型会非常暗，可用 `SKY_NIGHT_GAIN` 提亮
- 多云/降雨不会进入物理模型，`SKY_RAIN_DIMMING` 只是艺术压暗
- 地形只有贴图 × 顶点色 × lightmap，没有阴影、SSAO、水面效果或物理雾
- 未写深度的半透明物体（部分粒子、雨）会被天空覆盖；雨可通过 `rain.depth=true` 保留
- 其他维度（下界/末地）保持原版渲染
- 只验证了 1.20.1 / Iris 1.7.6 / Sodium 0.5.13 这一种组合

## 许可

GPL-3.0-only，与仓库一致。物理模型与求解器来自 sky-tracer 项目。
