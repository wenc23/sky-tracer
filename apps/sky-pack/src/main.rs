//! Bake the four-wave realtime atmosphere into an Iris/OptiFine shader pack and
//! validate it offline.
//!
//! `bake` exports the fixed-medium solver output from `sky-realtime` (optical
//! depth, directional multiple-scattering source and every coefficient table),
//! generates the shader constants, copies the tracked shader sources and writes
//! a complete pack. `preview` compiles the *actual shipped shader files* with a
//! real OpenGL compatibility context, renders them, and compares the result with
//! the crate's own per-pixel transport. It exists because Minecraft is not a
//! required build dependency of this research repository.

mod bake;
mod gl;
mod glsl;
mod harness;
mod pack;
mod preview;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "sky-pack",
    version,
    about = "Bake and validate the Sky Tracer Minecraft shader pack"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Export the solved atmosphere and write a complete shader pack.
    Bake(BakeArgs),
    /// Compile the shipped GLSL and compare it against the crate's renderer.
    Preview(PreviewArgs),
}

#[derive(Parser, Debug)]
pub struct BakeArgs {
    /// Output pack directory.
    #[arg(long, default_value = "out/shaderpacks/SkyTracer")]
    pub out: PathBuf,
    /// Tracked shader source template directory.
    #[arg(long, default_value = "mc/SkyTracer")]
    pub template: PathBuf,
    /// Aerosol mass multiplier of the fixed medium.
    #[arg(long, default_value_t = 1.0)]
    pub aerosol_scale: f32,
    /// Uniform ground albedo of the fixed medium.
    #[arg(long, default_value_t = 0.18)]
    pub ground_albedo: f32,
    /// Runtime transport steps baked into the shader.
    #[arg(long, default_value_t = sky_realtime::DEFAULT_RUNTIME_STEPS)]
    pub steps: u32,
}

#[derive(Parser, Debug)]
pub struct PreviewArgs {
    /// Pack produced by `bake`.
    #[arg(long, default_value = "out/shaderpacks/SkyTracer")]
    pub pack: PathBuf,
    /// Directory for preview images and validation reports.
    #[arg(long, default_value = "out/mc-preview")]
    pub out: PathBuf,
    /// Comparison image size.
    #[arg(long, default_value_t = 960)]
    pub width: u32,
    /// Comparison image height.
    #[arg(long, default_value_t = 540)]
    pub height: u32,
    /// Comma separated Sun elevations (degrees) to compare/preview.
    #[arg(long, default_value = "35,10,2,-2,-6,-12")]
    pub sun_elevations: String,
    /// Observer altitude in kilometres.
    #[arg(long, default_value_t = 0.07)]
    pub altitude_km: f32,
    /// Vertical field of view in degrees.
    #[arg(long, default_value_t = 90.0)]
    pub fov_y_deg: f32,
    /// Sky exposure multiplier used for the preview images.
    #[arg(long, default_value_t = 1.0)]
    pub exposure: f32,
    /// Skip PNG output (metrics only).
    #[arg(long, default_value_t = false)]
    pub metrics_only: bool,
}

fn main() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command {
        Command::Bake(args) => pollster::block_on(bake::run(args)),
        Command::Preview(args) => preview::run(args),
    }
}
