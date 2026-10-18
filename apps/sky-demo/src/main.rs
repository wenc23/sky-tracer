//! Reference/LUT/sky workbench entry point.
//!
//! The weather/cloud visualization workbench has its own entry point in
//! `apps/sky-weather-workbench`; both binaries share the display shell in this
//! crate's library.

use std::error::Error;

use clap::Parser;
use sky_demo::cli::{Cli, run};

fn main() -> Result<(), Box<dyn Error>> {
    run(Cli::parse())
}
