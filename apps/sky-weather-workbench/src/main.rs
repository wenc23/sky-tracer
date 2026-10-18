//! 实时天气 / 云可视化工作台入口。
//!
//! 这是一个 egui 窗口：实时渲染四波长天空、行为优先的体积云场与屏幕空间天气
//! 效果，并把所有行为 / 渲染参数暴露在右侧“设置面板”里。
//!
//! 它复用 `sky-demo` 库中的显示外壳（GPU 上下文、egui 集成、实验抽象、呈现
//! pass）；物理与天气行为仍在 `sky-*` crate 中，本二进制只负责参数、I/O、
//! 展示与实验统计。

use std::error::Error;
use std::path::PathBuf;

use clap::Parser;
use sky_demo::app::{AppMode, CaptureOptions, ExperimentKind, RunConfig, WeatherSession};
use sky_demo::controls::{
    CloudQuality, EffectsQuality, WeatherModeControl, climate_from_name, climate_label,
    weather_kind_from_name, weather_kind_label, weather_mode_from_name,
};

/// 实时天气 / 云可视化工作台（天空 + 体积云 + 屏幕空间天气效果）。
///
/// 所有选项只初始化工作台参数；启动后仍可在窗口右侧“设置面板”里交互修改。
/// `--smoke` 会用同一条实验 + UI 管线离屏渲染，写出一张 PNG 后自动退出，
/// 用于无人工操作的回归验证。
#[derive(Parser, Debug)]
#[command(
    version,
    about = "实时天气 / 云可视化工作台",
    long_about = None,
    after_help = "\
天气名称（--kind 的英文取值）：clear 晴空、fair 淡积云、partly_cloudy 多云、
overcast 阴天、fog 雾、haze 霾、dust 沙尘、drizzle 毛毛雨、rain 雨、
heavy_rain 大雨、tropical_shower 热带阵雨、thunderstorm 雷暴、hailstorm 冰雹、
supercell 强风暴、light_snow 小雪、heavy_snow 大雪、blizzard 暴风雪、
sleet 雨夹雪、freezing_rain 冻雨、gale 大风
气候（--climate 的英文取值）：tropical 热带、temperate 温带、continental 大陆性、
arid 干旱、polar 极地、alpine 高山
窗口内操作：拖动 = 视角，滚轮 = 缩放，空格 = 暂停，[ ] = 太阳高度，
, . = 太阳方位，F6 = HDR（支持时），R = 重置视角。"
)]
struct Cli {
    /// 天气模式：auto（自动演变）、fixed（固定天气）、tour（目录轮巡）。
    #[arg(long, default_value = "auto", value_parser = ["auto", "fixed", "tour"])]
    mode: String,
    /// 固定天气种类（在 --mode auto 下会隐含切到 fixed）。
    #[arg(
        long,
        value_parser = [
            "clear", "fair", "partly_cloudy", "overcast", "fog", "haze", "dust", "drizzle",
            "rain", "heavy_rain", "tropical_shower", "thunderstorm", "hailstorm", "supercell",
            "light_snow", "heavy_snow", "blizzard", "sleet", "freezing_rain", "gale"
        ]
    )]
    kind: Option<String>,
    /// 气候：tropical / temperate / continental / arid / polar / alpine。
    #[arg(
        long,
        default_value = "temperate",
        value_parser = ["tropical", "temperate", "continental", "arid", "polar", "alpine"]
    )]
    climate: String,
    /// 天气行为强度倍率（0.25..=2.5）。
    #[arg(long, default_value_t = 1.0, allow_hyphen_values = true)]
    intensity: f32,
    /// 每墙钟秒推进的模拟秒数（1..=600）。
    #[arg(long, default_value_t = 60.0)]
    speed: f32,
    /// 确定性行为种子。
    #[arg(long, default_value_t = sky_demo::controls::DEFAULT_WEATHER_SEED)]
    seed: u64,
    /// 行为季节：0 冬、0.5 春/秋、1 夏。
    #[arg(long, default_value_t = sky_demo::controls::DEFAULT_WEATHER_SEASON)]
    season: f32,
    /// 天气时钟太阳位置的纬度（-90..=90）。
    #[arg(long, default_value_t = sky_demo::controls::DEFAULT_WEATHER_LATITUDE_DEG)]
    latitude_deg: f32,
    /// 天气时钟太阳位置的年积日（1..=366）。
    #[arg(long, default_value_t = sky_demo::controls::DEFAULT_WEATHER_DAY_OF_YEAR)]
    day_of_year: f32,
    /// 天气时钟的起始 UTC 小时（0..24）。
    #[arg(long, default_value_t = sky_demo::controls::DEFAULT_WEATHER_START_HOUR_UTC)]
    start_hour_utc: f32,
    /// 由天气时钟驱动太阳（默认）。
    #[arg(long)]
    auto_sun: bool,
    /// 使用手动太阳高度/方位，而不是天气时钟。
    #[arg(long = "no-auto-sun", conflicts_with = "auto_sun")]
    no_auto_sun: bool,
    /// 手动太阳高度（配合 --no-auto-sun）。
    #[arg(long, default_value_t = 30.0, allow_hyphen_values = true)]
    sun_elevation_deg: f32,
    /// 手动太阳方位（配合 --no-auto-sun；0° 朝 +Z，90° 朝 +X）。
    #[arg(long, default_value_t = 180.0, allow_hyphen_values = true)]
    sun_azimuth_deg: f32,
    /// 观察海拔（km）。
    #[arg(long, default_value_t = 0.2)]
    observer_altitude_km: f32,
    /// 初始相机偏航角（°）。
    #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
    yaw_deg: f32,
    /// 初始相机俯仰角（°）。
    #[arg(long, default_value_t = 18.0, allow_hyphen_values = true)]
    pitch_deg: f32,
    /// 初始垂直视场角（°）。
    #[arg(long, default_value_t = 70.0)]
    fov_y_deg: f32,
    /// 体积云质量：balanced（0.8× 追踪分辨率，12 样本 / 32 步）或 high（0.85× 追踪，16 样本 / 40 步）。
    #[arg(long, default_value = "balanced", value_parser = ["balanced", "high"])]
    clouds: String,
    /// 屏幕空间天气效果质量：high 或 low。
    #[arg(long, default_value = "high", value_parser = ["high", "low"])]
    effects: String,
    /// 显示曝光补偿（EV）。
    #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
    exposure_ev: f32,
    /// 云层遮光强度（0..=1）：按云量/含水量压暗天空、地面与云底。
    #[arg(long, default_value_t = 1.0)]
    cloud_occlusion: f32,
    /// 首帧前的天气行为预热时长（模拟秒）。
    #[arg(long, default_value_t = 90.0)]
    warmup_seconds: f64,
    /// 离屏渲染 N 帧后写 PNG 并退出（冒烟截图）。
    #[arg(long)]
    smoke: Option<u32>,
    /// --smoke 的截图输出路径。
    #[arg(long)]
    screenshot: Option<PathBuf>,
    /// 离屏截图宽度（物理像素）。
    #[arg(long, default_value_t = 1600)]
    width: u32,
    /// 离屏截图高度（物理像素）。
    #[arg(long, default_value_t = 900)]
    height: u32,
}

fn parse_cloud_quality(name: &str) -> Result<CloudQuality, String> {
    match name {
        "balanced" => Ok(CloudQuality::Balanced),
        "high" => Ok(CloudQuality::High),
        other => Err(format!("未知的云质量 {other:?}；可用值为 balanced 或 high")),
    }
}

fn parse_effects_quality(name: &str) -> Result<EffectsQuality, String> {
    match name {
        "high" => Ok(EffectsQuality::High),
        "low" => Ok(EffectsQuality::Low),
        other => Err(format!("未知的效果质量 {other:?}；可用值为 high 或 low")),
    }
}

fn validate(cli: &Cli) -> Result<RunConfig, Box<dyn Error>> {
    if !cli.warmup_seconds.is_finite() || !(0.0..=86_400.0).contains(&cli.warmup_seconds) {
        return Err("--warmup-seconds 必须是 0..=86400 内的有限数值".into());
    }
    let mut mode = weather_mode_from_name(&cli.mode)
        .ok_or_else(|| format!("未知的天气模式 {:?}；可用值为 auto、fixed、tour", cli.mode))?;
    let kind = match &cli.kind {
        Some(name) => {
            let kind =
                weather_kind_from_name(name).ok_or_else(|| format!("未知的天气种类 {name:?}"))?;
            if mode == WeatherModeControl::Auto {
                mode = WeatherModeControl::Fixed;
            }
            kind
        }
        None => sky_demo::controls::WeatherKind::PartlyCloudy,
    };
    let climate =
        climate_from_name(&cli.climate).ok_or_else(|| format!("未知的气候 {:?}", cli.climate))?;
    let cloud_quality = parse_cloud_quality(&cli.clouds)?;
    let effects_quality = parse_effects_quality(&cli.effects)?;
    let capture = match cli.smoke {
        Some(frames) => {
            if frames == 0 {
                return Err("--smoke 至少需要 1 帧".into());
            }
            if frames > 10_000 {
                return Err("--smoke 帧数上限为 10000".into());
            }
            if cli.width < 320 || cli.height < 200 {
                return Err("--width/--height 至少为 320x200".into());
            }
            let screenshot = cli
                .screenshot
                .clone()
                .ok_or("--smoke 需要同时给出 --screenshot <路径>")?;
            Some(CaptureOptions {
                frames,
                screenshot,
                width: cli.width,
                height: cli.height,
            })
        }
        None => None,
    };
    let weather = WeatherSession {
        mode,
        kind,
        climate,
        intensity: cli.intensity,
        speed: cli.speed,
        seed: cli.seed,
        auto_sun: !cli.no_auto_sun,
        season: cli.season,
        latitude_deg: cli.latitude_deg,
        day_of_year: cli.day_of_year,
        start_hour_utc: cli.start_hour_utc,
        sun_elevation_deg: cli.sun_elevation_deg,
        sun_azimuth_deg: cli.sun_azimuth_deg,
        observer_altitude_km: cli.observer_altitude_km,
        yaw_deg: cli.yaw_deg,
        pitch_deg: cli.pitch_deg,
        fov_y_deg: cli.fov_y_deg,
        cloud_quality,
        effects_quality,
        exposure_ev: cli.exposure_ev,
        cloud_occlusion: cli.cloud_occlusion,
    };
    Ok(RunConfig {
        asset_path: None,
        experiment: ExperimentKind::Weather,
        lut_path: PathBuf::from("out/lut_reference_v6_packed16"),
        mode: AppMode::Weather,
        weather: Some(weather),
        capture,
        warmup_seconds: cli.warmup_seconds,
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let config = validate(&cli)?;
    let weather = config
        .weather
        .as_ref()
        .expect("天气工作台总会配置一个天气会话");
    println!(
        "sky-weather-workbench：模式={} 天气={} 气候={} 种子={} 季节={:.2} \
         自动太阳={} 速度={:.0}x 强度={:.2}",
        weather.mode.label_zh(),
        weather_kind_label(weather.kind),
        climate_label(weather.climate),
        weather.seed,
        weather.season,
        weather.auto_sun,
        weather.speed,
        weather.intensity,
    );
    if let Some(capture) = &config.capture {
        println!(
            "冒烟截图：{} 帧 -> {}（{}x{}）",
            capture.frames,
            capture.screenshot.display(),
            capture.width,
            capture.height
        );
    }
    sky_demo::app::run(config)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, validate};

    fn base() -> Cli {
        Cli::parse_from(["sky-weather-workbench"])
    }

    #[test]
    fn defaults_describe_an_evolving_temperate_weather_workbench() {
        let config = validate(&base()).expect("defaults validate");
        let weather = config.weather.expect("weather session");
        assert_eq!(config.mode, sky_demo::app::AppMode::Weather);
        assert!(weather.auto_sun);
        assert!(config.capture.is_none());
    }

    #[test]
    fn fixed_kind_implies_fixed_mode() {
        let mut cli = base();
        cli.kind = Some("thunderstorm".to_owned());
        let weather = validate(&cli).expect("valid").weather.expect("session");
        assert_eq!(weather.mode, sky_demo::controls::WeatherModeControl::Fixed);
        assert_eq!(weather.kind, sky_demo::controls::WeatherKind::Thunderstorm);
    }

    #[test]
    fn smoke_requires_screenshot() {
        let mut cli = base();
        cli.smoke = Some(4);
        assert!(validate(&cli).is_err());
        cli.screenshot = Some("out/test.png".into());
        let config = validate(&cli).expect("valid capture");
        assert_eq!(config.capture.expect("capture").frames, 4);
    }

    #[test]
    fn no_auto_sun_disables_the_weather_clock_sun() {
        let mut cli = base();
        cli.no_auto_sun = true;
        let weather = validate(&cli).expect("valid").weather.expect("session");
        assert!(!weather.auto_sun);
    }

    #[test]
    fn unknown_inputs_are_rejected() {
        let mut cli = base();
        cli.kind = Some("tornado".to_owned());
        assert!(validate(&cli).is_err());
        let mut cli = base();
        cli.climate = "mars".to_owned();
        assert!(validate(&cli).is_err());
    }
}
