//! Command-line entry point for the reference/LUT workbench binary.

use std::{error::Error, path::PathBuf};

use clap::Parser;

use crate::app::{self, AppMode, RunConfig};

#[derive(Parser, Debug)]
#[command(version, about = "Realtime atmosphere experiment demo")]
pub struct Cli {
    #[arg(long)]
    pub asset: Option<PathBuf>,
    #[arg(long, value_enum)]
    pub experiment: Option<app::ExperimentKind>,
    /// Baked reference LUT directory for --experiment reference.
    #[arg(long)]
    pub lut: Option<PathBuf>,
    /// Refuse oversized LUT payloads before creating a GPU device. Excludes
    /// frame targets and other GPU allocations; 0 disables this guard.
    #[arg(long, default_value_t = 1024)]
    pub lut_budget_mib: u64,
    /// Save LUT/reference/absolute/signed comparison panels without opening a window.
    #[arg(long)]
    pub snapshot: Option<PathBuf>,
    /// Export four raw linear Rec.2020 f32 panels and a JSON sidecar.
    #[arg(long)]
    pub snapshot_linear: bool,
    /// Measure GPU timestamps for camera changes, sun changes, and cached frames.
    #[arg(long, default_value_t = 0)]
    pub benchmark_frames: usize,
    #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
    pub snapshot_yaw_deg: f32,
    #[arg(long, default_value_t = 15.0, allow_hyphen_values = true)]
    pub snapshot_pitch_deg: f32,
    #[arg(long, default_value_t = 90.0)]
    pub snapshot_fov_deg: f32,
    #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
    pub snapshot_exposure_ev: f32,
    /// Weather snapshot mode: auto, fixed, or tour.
    #[arg(long)]
    pub snapshot_weather_mode: Option<String>,
    /// Fixed weather kind for a weather snapshot, e.g. rain or heavy_snow.
    #[arg(long)]
    pub snapshot_weather_kind: Option<String>,
    /// Climate for a weather snapshot: tropical, temperate, continental, arid, polar, alpine.
    #[arg(long)]
    pub snapshot_weather_climate: Option<String>,
    /// Weather seed for a weather snapshot.
    #[arg(long)]
    pub snapshot_weather_seed: Option<u64>,
    /// Behaviour spin-up before the weather snapshot.
    #[arg(long, default_value_t = 300.0)]
    pub snapshot_weather_spinup_s: f64,
}

pub fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    let experiment = cli.experiment.unwrap_or(app::ExperimentKind::Hybrid4d);
    let lut = cli
        .lut
        .unwrap_or_else(|| PathBuf::from("out/lut_reference_v6_packed16"));
    if matches!(experiment, app::ExperimentKind::Hybrid4d) {
        let model = sky_realtime::model::Model::earth().map_err(std::io::Error::other)?;
        let resident = sky_realtime::estimated_resident_bytes(
            &model,
            &sky_realtime::Wavelengths::optimized_four(),
            0.18,
            &sky_realtime::Config::balanced(),
        )
        .map_err(std::io::Error::other)?;
        if cli.lut_budget_mib != 0 && resident > cli.lut_budget_mib.saturating_mul(1048576) {
            return Err(format!(
                "Hybrid resident LUTs including SkyView need {:.3} MiB, above --lut-budget-mib {}",
                resident as f32 / 1048576.0,
                cli.lut_budget_mib
            )
            .into());
        }
    }
    if matches!(experiment, app::ExperimentKind::OfflineLut) {
        let manifest = sky_reference::asset::Manifest::open(&lut).map_err(std::io::Error::other)?;
        let (_, bytes) = sky_reference::renderer::storage_budget(&manifest);
        if cli.lut_budget_mib != 0 && bytes > cli.lut_budget_mib.saturating_mul(1048576) {
            return Err(format!("LUT needs {:.1} MiB of GPU payload, above --lut-budget-mib {}. Use the CPU-packed resource (--lut out/lut_reference_v6_packed16), or export-rgb then compress-rgb with sky-baker reference. Raise --lut-budget-mib explicitly only when enough VRAM is available.",bytes as f32/1048576.0,cli.lut_budget_mib).into());
        }
    }
    if let Some(output) = cli.snapshot {
        if cli.asset.is_none() && !matches!(experiment, app::ExperimentKind::Weather) {
            return Err("--snapshot requires --asset for PT comparison".into());
        }
        return crate::snapshot::render(
            cli.asset.as_deref(),
            &lut,
            &output,
            experiment,
            cli.snapshot_linear,
            [
                cli.snapshot_yaw_deg,
                cli.snapshot_pitch_deg,
                cli.snapshot_fov_deg,
                cli.snapshot_exposure_ev,
            ],
            cli.benchmark_frames,
            &crate::snapshot::WeatherSnapshotOptions {
                mode: cli.snapshot_weather_mode,
                kind: cli.snapshot_weather_kind,
                climate: cli.snapshot_weather_climate,
                seed: cli.snapshot_weather_seed,
                spinup_s: cli.snapshot_weather_spinup_s,
            },
        );
    }
    app::run(RunConfig {
        asset_path: cli.asset,
        experiment,
        lut_path: lut,
        mode: AppMode::Reference,
        weather: None,
        capture: None,
        warmup_seconds: 0.0,
    })
}
