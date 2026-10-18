//! Reusable real-time display shell for sky-tracer.
//!
//! The `sky-demo` binary is the reference/PT workbench; other display
//! applications (currently `sky-weather-workbench`) reuse the GPU context,
//! egui integration, experiment abstraction and presentation pass from this
//! library. Solver code stays in the `sky-*` crates; this crate only owns
//! parameters, I/O, presentation and experiment statistics.

pub mod app;
pub mod assets;
pub mod catalog;
pub mod cli;
pub mod color;
pub mod controls;
pub mod experiment;
pub mod gpu;
pub mod passes;
pub mod snapshot;
pub mod ui_renderer;
pub mod view;
pub mod workbench;
