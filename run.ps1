# sky-tracer 统一入口（Windows PowerShell 5.1+）
#
# 建议通过 run.cmd 调用（自动选择 pwsh / Windows PowerShell，处理执行策略并传递退出码）：
#   .\run.cmd                 交互菜单
#   .\run.cmd workbench       天气可视化工作台 · 实时（默认自动天气）
#   .\run.cmd weather         同上
#   .\run.cmd fixed [kind]    天气可视化工作台 · 固定天气（20 种之一，可给名称或 1-20 序号）
#   .\run.cmd smoke [path]    天气可视化工作台 · 冒烟截图（离屏渲染同一管线 + UI，自动退出）
#   .\run.cmd sky             天空实时工作台（四维多散射）
#   .\run.cmd realtime        同上
#   .\run.cmd snapshot        天气/云快照（无窗口）
#   .\run.cmd audit           天气行为审计（weather-catalog + weather-simulate）
#   .\run.cmd effects         天气效果与云预览审计（effects-zero + render-cloud）
#   .\run.cmd mc              Minecraft 光影包（bake → preview）
#   .\run.cmd mc-bake         仅烘焙 Minecraft 光影包
#   .\run.cmd mc-preview      仅离线 GLSL/像素验证（缺包时先 bake）
#   .\run.cmd check           架构检查 + cargo test 说明
#   .\run.cmd help            显示帮助
#
# 也可以直接调用（需要执行策略允许）：
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\run.ps1 smoke
#
# 新工作台 CLI（冻结契约；构建后以 --help 为准）：
#   --mode <auto|fixed|tour>       --kind <20 种天气之一>（指定 kind 时隐含 fixed）
#   --climate <tropical|temperate|continental|arid|polar|alpine>
#   --intensity <0.25..2.5>        --speed <1..600>
#   --seed <u64>                   --season <0..1>
#   --latitude-deg <度数>          --day-of-year <1..366>
#   --start-hour-utc <0..24>       --auto-sun | --no-auto-sun
#   --sun-elevation-deg <度数>     --sun-azimuth-deg <度数>
#   --observer-altitude-km <km>    --yaw-deg / --pitch-deg / --fov-y-deg
#   --clouds <balanced|high>       --effects <high|low>
#   --exposure-ev <EV>             --warmup-seconds <模拟秒数>
#   --smoke <FRAMES>               --screenshot <PATH>（--smoke 必须给 screenshot）
#   --width <u32> / --height <u32>（离屏截图尺寸，仅冒烟；默认 1600x900）
# 默认值：mode=auto, climate=temperate, intensity=1.0, speed=60, season=0.55,
# latitude=45, day-of-year=172, start-hour-utc=9, auto-sun 开启, sun=(30,180),
# observer=0.2 km, yaw/pitch/fov=(0,18,70), clouds=balanced, effects=high,
# exposure-ev=0, warmup-seconds=90。
# 窗口按键：拖拽=视角、滚轮=缩放、Space=暂停、[ ]=太阳高度、, .=太阳方位、
# F6=HDR（支持时）、R=重置视角；标题为「天气可视化工作台」（中文界面）。
#
# 说明：
# - 所有条目都用 cargo run --release，首次运行会自动 release 构建（可能数分钟）。
# - 工作台/快照/云图/效果/Minecraft 验证都会创建 GPU 设备，不要并行运行。
# - 新工作台入口 apps/sky-weather-workbench 已接入 workspace；冒烟截图会离屏渲染同一
#   管线 + UI、写 PNG、打印稳态帧时间后自动退出，适合无人工操作的回归验证。

[CmdletBinding()]
param(
    [string]$Choice = "",
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Extra = @()
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $Root

if ($null -eq $Extra) { $Extra = @() }

$script:KeepGoing = $true
$script:Interactive = $false
$script:LastCode = 0

# 20 种文档天气（与 sky-weather::WeatherKind 目录一致，见 docs/weather.md）。
$script:Kinds = @(
    "clear", "fair", "partly_cloudy", "overcast", "fog", "haze", "dust",
    "drizzle", "rain", "heavy_rain", "tropical_shower", "thunderstorm",
    "hailstorm", "supercell", "light_snow", "heavy_snow", "blizzard",
    "sleet", "freezing_rain", "gale"
)

$script:KindZh = @{
    "clear" = "晴空"; "fair" = "少量淡积云"; "partly_cloudy" = "多云积云"; "overcast" = "阴天层云"
    "fog" = "接地雾"; "haze" = "干霾"; "dust" = "沙尘"; "drizzle" = "毛毛雨"
    "rain" = "雨"; "heavy_rain" = "大雨"; "tropical_shower" = "暖性阵雨"; "thunderstorm" = "雷暴"
    "hailstorm" = "冰雹"; "supercell" = "强风暴"; "light_snow" = "小雪"; "heavy_snow" = "大雪"
    "blizzard" = "暴风雪"; "sleet" = "雨夹雪"; "freezing_rain" = "冻雨"; "gale" = "大风"
}

function Write-Step([string]$Text) {
    Write-Host ""
    Write-Host (">>> " + $Text) -ForegroundColor Green
}

function Write-Note([string]$Text) {
    Write-Host $Text -ForegroundColor DarkGray
}

function Write-Warn([string]$Text) {
    Write-Host $Text -ForegroundColor Yellow
}

function Write-Fail([string]$Text) {
    Write-Host $Text -ForegroundColor Red
}

# 首次运行提示：cargo run --release 会自动构建，这里只提示缺失的 release 二进制。
function Show-BuildHint([string]$Binary) {
    $exe = Join-Path (Join-Path "target" "release") ($Binary + ".exe")
    if (-not (Test-Path $exe)) {
        Write-Note ("（未找到 " + $exe + "：首次运行会自动 release 构建，可能数分钟）")
    }
}

function Invoke-Native([string[]]$CargoArgs) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-Fail "未找到 cargo。请先安装 Rust 工具链（https://rustup.rs），并确保 cargo 在 PATH 中。"
        $script:LastCode = 127
        return
    }
    & cargo @CargoArgs
    $script:LastCode = $LASTEXITCODE
}

function Show-Kinds {
    Write-Host ""
    Write-Host "20 种天气目录（固定模式可用名称或序号）：" -ForegroundColor Cyan
    for ($i = 0; $i -lt $script:Kinds.Count; $i++) {
        $name = $script:Kinds[$i]
        Write-Host ("  {0,2}) {1,-15} {2}" -f ($i + 1), $name, $script:KindZh[$name])
    }
    Write-Host ""
}

function Resolve-WeatherKind([string]$Raw) {
    if ([string]::IsNullOrWhiteSpace($Raw)) { return $null }
    $name = $Raw.Trim().ToLowerInvariant()
    if ($script:Kinds -contains $name) { return $name }
    if ($name -match '^[0-9]+$') {
        $index = [int]$name - 1
        if ($index -ge 0 -and $index -lt $script:Kinds.Count) {
            return $script:Kinds[$index]
        }
    }
    return $null
}

function Ensure-Parent([string]$Path) {
    $dir = Split-Path -Parent $Path
    if ([string]::IsNullOrEmpty($dir)) { $dir = "." }
    if (-not (Test-Path $dir)) {
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
    }
}

function Invoke-Workbench([string[]]$AppArgs) {
    Write-Step "天气可视化工作台 · 实时（默认自动天气；首次运行会自动 release 构建）"
    Show-BuildHint "sky-weather-workbench"
    $cargoArgs = @("run", "--release", "-p", "sky-weather-workbench")
    if ($AppArgs.Count -gt 0) {
        $cargoArgs = $cargoArgs + @("--") + $AppArgs
    }
    Invoke-Native $cargoArgs
}

function Invoke-Fixed([string]$RawKind, [string[]]$AppArgs) {
    $kind = Resolve-WeatherKind $RawKind
    if ($null -eq $kind) {
        if ([string]::IsNullOrWhiteSpace($RawKind)) {
            Write-Fail "固定天气需要指定 20 种天气之一（名称或 1-20 序号）。"
        } else {
            Write-Fail ("未知天气：'" + $RawKind + "'。固定模式只接受 20 种文档天气之一。")
        }
        Show-Kinds
        $script:LastCode = 2
        return
    }
    Write-Step ("天气可视化工作台 · 固定天气 " + $kind + "（" + $script:KindZh[$kind] + "；行为优先，不是实测天气）")
    Show-BuildHint "sky-weather-workbench"
    # 注意：PowerShell 变量名不区分大小写，局部变量不能与参数 $AppArgs 同名。
    $builtArgs = @("--mode", "fixed", "--kind", $kind)
    if ($AppArgs.Count -gt 0) { $builtArgs = $builtArgs + $AppArgs }
    $cargoArgs = @("run", "--release", "-p", "sky-weather-workbench", "--") + $builtArgs
    Invoke-Native $cargoArgs
}

function Invoke-Smoke([string]$OutPath) {
    $path = $OutPath
    if ([string]::IsNullOrWhiteSpace($path)) { $path = "out/workbench-weather-smoke.png" }
    Ensure-Parent $path
    Write-Step "天气可视化工作台 · 冒烟截图（无人工操作：离屏渲染同一管线 + UI 后自动退出）"
    Write-Note ("截图输出：" + $path + "（1600x900）")
    Show-BuildHint "sky-weather-workbench"
    $cargoArgs = @(
        "run", "--release", "-p", "sky-weather-workbench", "--",
        "--smoke", "24",
        "--warmup-seconds", "120",
        "--screenshot", $path,
        "--width", "1600",
        "--height", "900"
    )
    Invoke-Native $cargoArgs
}

function Invoke-Sky([string[]]$AppArgs) {
    Write-Step "天空实时工作台（四维多散射；天空/参考 LUT/PT 对照仍在 sky-demo）"
    Show-BuildHint "sky-demo"
    $cargoArgs = @("run", "--release", "-p", "sky-demo")
    if ($AppArgs.Count -gt 0) {
        $cargoArgs = $cargoArgs + @("--") + $AppArgs
    }
    Invoke-Native $cargoArgs
}

function Invoke-Snapshot([string[]]$AppArgs) {
    $path = "out/workbench-weather-snapshot.png"
    Ensure-Parent $path
    Write-Step "天气/云快照（无窗口；行为截图，不是辐射度量参照）"
    Write-Note ("截图输出：" + $path)
    Show-BuildHint "sky-demo"
    # 注意：PowerShell 变量名不区分大小写，局部变量不能与参数 $AppArgs 同名。
    $builtArgs = @(
        "--experiment", "weather",
        "--snapshot", $path,
        "--snapshot-pitch-deg", "20",
        "--snapshot-weather-spinup-s", "120"
    )
    if ($AppArgs.Count -gt 0) { $builtArgs = $builtArgs + $AppArgs }
    $cargoArgs = @("run", "--release", "-p", "sky-demo", "--") + $builtArgs
    Invoke-Native $cargoArgs
}

function Invoke-Audit([string]$Which) {
    $mode = $Which.ToLowerInvariant()
    if (@("catalog", "simulate", "both") -notcontains $mode) { $mode = "both" }
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    if ($mode -eq "catalog" -or $mode -eq "both") {
        Write-Step "天气行为审计 · 20 种目录（cloud-audit weather-catalog；CPU 行为计算，不渲染、不需要 GPU）"
        Show-BuildHint "cloud-audit"
        $out = "out/weather-catalog-" + $stamp
        Invoke-Native @("run", "--release", "-p", "cloud-audit", "--", "weather-catalog", "--out", $out, "--seconds", "120")
        if ($script:LastCode -ne 0) { return }
        Write-Note ("输出：" + $out + "（20 种状态各推进 120 s 的 frame/effects/volume/finite 诊断）")
    }
    if ($mode -eq "simulate" -or $mode -eq "both") {
        Write-Step "天气行为审计 · Auto 演变（cloud-audit weather-simulate，温带 6 小时模拟）"
        Show-BuildHint "cloud-audit"
        $out = "out/weather-simulate-" + $stamp
        Invoke-Native @("run", "--release", "-p", "cloud-audit", "--", "weather-simulate", "--climate", "temperate", "--hours", "6", "--chunk-s", "300", "--out", $out)
        if ($script:LastCode -ne 0) { return }
        Write-Note ("输出：" + $out + "（Auto 状态转换记录）")
    }
}

function Invoke-Effects([string]$Which) {
    $mode = $Which.ToLowerInvariant()
    if (@("zero", "cloud", "both") -notcontains $mode) { $mode = "both" }
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    if ($mode -eq "zero" -or $mode -eq "both") {
        Write-Step "天气效果审计 · GPU 零输入恒等式（cloud-audit effects-zero；创建 wgpu 设备，需串行）"
        Show-BuildHint "cloud-audit"
        $out = "out/effects-zero-" + $stamp
        Invoke-Native @("run", "--release", "-p", "cloud-audit", "--", "effects-zero", "--out", $out, "--size", "64")
        if ($script:LastCode -ne 0) { return }
        Write-Note ("输出：" + $out + "（零效果应逐像素等于背景）")
    }
    if ($mode -eq "cloud" -or $mode -eq "both") {
        Write-Step "天气效果审计 · 无窗口云图（cloud-audit render-cloud --kind rain 320x180）"
        Show-BuildHint "cloud-audit"
        $out = "out/cloud-preview-" + $stamp
        Invoke-Native @("run", "--release", "-p", "cloud-audit", "--", "render-cloud", "--kind", "rain", "--out", $out, "--width", "320", "--height", "180")
        if ($script:LastCode -ne 0) { return }
        Write-Note ("输出：" + $out + "（云图 PNG + JSON 诊断）")
    }
}

function Invoke-Minecraft([string]$Which) {
    $mode = $Which.ToLowerInvariant()
    if (@("bake", "preview", "both") -notcontains $mode) { $mode = "both" }
    $packDir = "out/shaderpacks/SkyTracer"
    if ($mode -eq "bake" -or $mode -eq "both") {
        Write-Step "Minecraft 光影包 · 烘焙（sky-pack bake；写到 out/shaderpacks/SkyTracer 与同名 zip）"
        Show-BuildHint "sky-pack"
        Invoke-Native @("run", "--release", "-p", "sky-pack", "--", "bake")
        if ($script:LastCode -ne 0) { return }
        Write-Note "输出：out/shaderpacks/SkyTracer"
    }
    if ($mode -eq "preview" -or $mode -eq "both") {
        if (-not (Test-Path (Join-Path $packDir "manifest.json"))) {
            Write-Warn ("未找到已烘焙的光影包（" + $packDir + "\manifest.json）。")
            if ($script:Interactive) {
                $answer = Read-Host "现在烘焙？(y/N)"
                if ($answer -ne "y" -and $answer -ne "Y") {
                    $script:LastCode = 2
                    return
                }
                Invoke-Native @("run", "--release", "-p", "sky-pack", "--", "bake")
                if ($script:LastCode -ne 0) { return }
            } else {
                Write-Warn "请先运行 .\run.cmd mc-bake，再运行 mc-preview。"
                $script:LastCode = 2
                return
            }
        }
        Write-Step "Minecraft 光影包 · 离线验证（sky-pack preview；本机 OpenGL 编译 GLSL 并与逐像素输运对照，不启动游戏）"
        Show-BuildHint "sky-pack"
        Invoke-Native @("run", "--release", "-p", "sky-pack", "--", "preview")
    }
}

function Invoke-Check {
    Write-Step "架构检查（scripts/check_architecture.py；只做静态所有权检查，不启动 GPU）"
    $pythonCmd = $null
    $pythonPrefix = @()
    if (Get-Command python -ErrorAction SilentlyContinue) {
        $pythonCmd = "python"
    } elseif (Get-Command py -ErrorAction SilentlyContinue) {
        $pythonCmd = "py"
        $pythonPrefix = @("-3")
    }
    if ($null -eq $pythonCmd) {
        Write-Warn "未找到 python/py，跳过架构检查；可手动运行 py -3 scripts/check_architecture.py"
    } else {
        & $pythonCmd @pythonPrefix scripts/check_architecture.py
        $script:LastCode = $LASTEXITCODE
        if ($script:LastCode -ne 0) {
            Write-Fail ("架构检查失败（退出码 " + $script:LastCode + "）。")
            return
        }
    }
    Write-Host ""
    Write-Host ">>> 后续回归命令（本条目只做架构检查，cargo test 需手动运行或在此菜单确认）" -ForegroundColor Green
    Write-Host "  cargo test --workspace --lib --bins --tests    # 全 workspace 测试"
    Write-Host "  cargo fmt --all -- --check                     # 格式检查（只报告，不改文件）"
    Write-Host "  cargo check --workspace --all-targets          # 全 target 类型检查"
    Write-Host "  python scripts/check_architecture.py           # 独立核心与数据所有权"
    if ($script:Interactive) {
        Write-Host ""
        $answer = Read-Host "现在运行 cargo test --workspace --lib --bins --tests？(y/N)"
        if ($answer -eq "y" -or $answer -eq "Y") {
            Write-Step "cargo test --workspace --lib --bins --tests"
            Invoke-Native @("test", "--workspace", "--lib", "--bins", "--tests")
        }
    }
}

function Invoke-OneShot([string]$Action, [string[]]$Rest) {
    switch ($Action) {
        "workbench" {
            $appArgs = $Rest
            if ($appArgs.Count -eq 1 -and (@("help", "-h") -contains $appArgs[0])) {
                $appArgs = @("--help")
            }
            Invoke-Workbench $appArgs
        }
        "fixed" {
            if ($Rest.Count -ge 1 -and (@("help", "-h", "--help") -contains $Rest[0])) {
                Invoke-Workbench @("--help")
                return
            }
            $kind = ""
            $appArgs = @()
            if ($Rest.Count -ge 1) {
                $kind = $Rest[0]
                if ($Rest.Count -gt 1) {
                    $appArgs = $Rest[1..($Rest.Count - 1)]
                }
            }
            if ([string]::IsNullOrWhiteSpace($kind)) {
                $kind = Read-Host "输入天气名称或序号（1-20；回车=clear；? 查看目录）"
                if ($kind -eq "?") {
                    Show-Kinds
                    $kind = Read-Host "输入天气名称或序号（回车=clear）"
                }
                if ([string]::IsNullOrWhiteSpace($kind)) { $kind = "clear" }
            }
            Invoke-Fixed $kind $appArgs
        }
        "smoke" {
            $path = ""
            if ($Rest.Count -ge 1) {
                if ($Rest[0].StartsWith("-")) {
                    Write-Warn ("忽略无法识别的参数：" + ($Rest -join " ") + "（冒烟命令固定为 --smoke 24 --warmup-seconds 120 --screenshot ... --width 1600 --height 900）")
                } else {
                    $path = $Rest[0]
                }
            }
            Invoke-Smoke $path
        }
        "sky" { Invoke-Sky $Rest }
        "snapshot" { Invoke-Snapshot $Rest }
        "audit" { Invoke-Audit "both" }
        "audit-catalog" { Invoke-Audit "catalog" }
        "audit-simulate" { Invoke-Audit "simulate" }
        "effects" { Invoke-Effects "both" }
        "effects-zero" { Invoke-Effects "zero" }
        "render-cloud" { Invoke-Effects "cloud" }
        "mc" { Invoke-Minecraft "both" }
        "mc-bake" { Invoke-Minecraft "bake" }
        "mc-preview" { Invoke-Minecraft "preview" }
        "check" { Invoke-Check }
        default {
            Write-Fail ("未知命令：" + $Action)
            $script:LastCode = 2
        }
    }
}

function Invoke-MenuChoice([string]$Key) {
    switch ($Key) {
        "1" { Invoke-OneShot "workbench" @() }
        "2" { Invoke-OneShot "fixed" @() }
        "3" { Invoke-OneShot "smoke" @() }
        "4" { Invoke-OneShot "sky" @() }
        "5" { Invoke-OneShot "snapshot" @() }
        "6" { Invoke-OneShot "audit" @() }
        "7" { Invoke-OneShot "effects" @() }
        "8" { Invoke-OneShot "mc" @() }
        "9" { Invoke-OneShot "check" @() }
        "0" {
            $script:KeepGoing = $false
            $script:LastCode = 0
        }
        default {
            Write-Fail ("未知选项：" + $Key)
            $script:LastCode = 2
        }
    }
}

function Show-Menu {
    Write-Host ""
    Write-Host "==== sky-tracer · 统一入口 ====" -ForegroundColor Cyan
    Write-Host "  1) 天气可视化工作台 · 实时（推荐；默认自动天气）"
    Write-Host "  2) 天气可视化工作台 · 固定天气（20 种之一，交互输入）"
    Write-Host "  3) 天气可视化工作台 · 冒烟截图（离屏渲染，自动退出）"
    Write-Host "  4) 天空实时工作台（四维多散射）"
    Write-Host "  5) 天气/云快照（无窗口，行为截图）"
    Write-Host "  6) 天气行为审计（20 种目录 + Auto 演变，CPU）"
    Write-Host "  7) 天气效果与云预览审计（GPU：effects-zero + render-cloud）"
    Write-Host "  8) Minecraft 光影包（bake → preview）"
    Write-Host "  9) 检查（架构检查 + cargo test 说明）"
    Write-Host "  0) 退出"
    Write-Host ""
    Write-Host "提示：首次运行会自动 release 构建（可能数分钟）；详细选项见 .\run.cmd help。" -ForegroundColor DarkGray
    Write-Host ""
}

function Show-Help {
    Write-Host ""
    Write-Host "sky-tracer 统一入口（run.cmd / run.ps1）" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "用法: .\run.cmd [命令] [参数...]"
    Write-Host "不带参数运行进入交互菜单。额外参数会原样传给被调用的程序。"
    Write-Host ""
    Write-Host "命令（一次运行）:" -ForegroundColor Cyan
    Write-Host "  workbench, weather    天气可视化工作台 · 实时（默认自动天气）"
    Write-Host "  fixed [kind]          天气可视化工作台 · 固定天气；kind 为 20 种之一或序号 1-20"
    Write-Host "  smoke [path]          冒烟截图；默认 out/workbench-weather-smoke.png"
    Write-Host "  sky, realtime         天空实时工作台（四维多散射）"
    Write-Host "  snapshot              天气/云快照（无窗口）"
    Write-Host "  audit                 天气行为审计（weather-catalog + weather-simulate）"
    Write-Host "  audit-catalog         仅 20 种天气目录"
    Write-Host "  audit-simulate        仅 Auto 演变"
    Write-Host "  effects               天气效果与云预览审计（effects-zero + render-cloud）"
    Write-Host "  effects-zero          仅 GPU 零效果恒等式"
    Write-Host "  render-cloud          仅无窗口云图"
    Write-Host "  mc                    Minecraft 光影包：bake → preview"
    Write-Host "  mc-bake               仅烘焙光影包"
    Write-Host "  mc-preview, mc-check  仅离线 GLSL/像素验证（缺包时自动先 bake）"
    Write-Host "  check                 架构检查 + cargo test 说明"
    Write-Host "  help                  显示本帮助"
    Write-Host ""
    Write-Host "示例:" -ForegroundColor Cyan
    Write-Host "  .\run.cmd workbench --mode tour --speed 60"
    Write-Host "  .\run.cmd fixed thunderstorm --climate polar"
    Write-Host "  .\run.cmd smoke out/workbench-smoke.png"
    Write-Host "  .\run.cmd sky --experiment reference --lut out/lut_reference_v6_packed16"
    Write-Host ""
    Write-Host "20 种天气:" -ForegroundColor Cyan
    Write-Host ("  " + ($script:Kinds -join ", "))
    Write-Host ""
    Write-Host "新工作台 CLI（冻结契约；构建后以 --help 为准）:" -ForegroundColor Cyan
    Write-Host "  --mode <auto|fixed|tour>       --kind <20 种之一>（指定 kind 隐含 fixed）"
    Write-Host "  --climate <tropical|temperate|continental|arid|polar|alpine>"
    Write-Host "  --intensity <0.25..2.5>        --speed <1..600>"
    Write-Host "  --seed <u64>                   --season <0..1>"
    Write-Host "  --latitude-deg <度数>          --day-of-year <1..366>"
    Write-Host "  --start-hour-utc <0..24>       --auto-sun | --no-auto-sun"
    Write-Host "  --sun-elevation-deg <度数>     --sun-azimuth-deg <度数>"
    Write-Host "  --observer-altitude-km <km>    --yaw-deg / --pitch-deg / --fov-y-deg"
    Write-Host "  --clouds <balanced|high>       --effects <high|low>"
    Write-Host "  --exposure-ev <EV>             --warmup-seconds <模拟秒数>"
    Write-Host "  --smoke <FRAMES>               --screenshot <PATH>（--smoke 必须给 screenshot）"
    Write-Host "  --width <u32> / --height <u32>（离屏截图尺寸，仅冒烟；默认 1600x900）"
    Write-Host "  默认：mode=auto climate=temperate intensity=1 speed=60 season=0.55"
    Write-Host "        latitude=45 day-of-year=172 start-hour-utc=9 auto-sun 开"
    Write-Host "        sun=(30,180) observer=0.2km yaw/pitch/fov=(0,18,70)"
    Write-Host "        clouds=balanced effects=high exposure-ev=0 warmup-seconds=90"
    Write-Host ""
    Write-Host "窗口按键:" -ForegroundColor Cyan
    Write-Host "  拖拽=视角  滚轮=缩放  Space=暂停  [ ]=太阳高度  , .=太阳方位"
    Write-Host "  F6=HDR（支持时）  R=重置视角；标题为「天气可视化工作台」"
    Write-Host ""
}

# ---- 主流程 ----

if ([string]::IsNullOrWhiteSpace($Choice)) {
    $script:Interactive = $true
    while ($script:KeepGoing) {
        Show-Menu
        $key = Read-Host "请选择"
        $key = $key.Trim()
        if ($key -eq "") { continue }
        Invoke-MenuChoice $key
        if ($script:KeepGoing -and $script:LastCode -ne 0) {
            Write-Warn ("上一步退出码：" + $script:LastCode)
        }
        if ($script:KeepGoing) {
            Write-Host ""
            $null = Read-Host "按回车返回菜单（Ctrl+C 退出）"
        }
    }
    exit 0
}

$key = $Choice.Trim().ToLowerInvariant()

if (@("help", "-h", "--help", "?") -contains $key) {
    Show-Help
    exit 0
}

$Aliases = @{
    "workbench" = "workbench"; "weather" = "workbench"
    "fixed" = "fixed"
    "smoke" = "smoke"
    "sky" = "sky"; "realtime" = "sky"
    "snapshot" = "snapshot"
    "audit" = "audit"; "audit-catalog" = "audit-catalog"; "audit-simulate" = "audit-simulate"
    "effects" = "effects"; "effects-zero" = "effects-zero"; "render-cloud" = "render-cloud"
    "mc" = "mc"; "mc-bake" = "mc-bake"; "mc-preview" = "mc-preview"; "mc-check" = "mc-preview"
    "check" = "check"
}

if (-not $Aliases.ContainsKey($key)) {
    Write-Fail ("未知命令：" + $Choice)
    Write-Host "可用命令见：.\run.cmd help" -ForegroundColor Yellow
    exit 2
}

Invoke-OneShot $Aliases[$key] $Extra
exit $script:LastCode
