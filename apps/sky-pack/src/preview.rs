use crate::PreviewArgs;
use crate::harness;

pub fn run(args: PreviewArgs) -> Result<(), String> {
    let report = pollster::block_on(harness::run(&args))?;
    std::fs::create_dir_all(&args.out).map_err(|e| e.to_string())?;
    let report_path = args.out.join("report.json");
    std::fs::write(
        &report_path,
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write {}: {e}", report_path.display()))?;

    println!("validated shader pack against sky-realtime direct transport");
    println!("GL renderer: {}", report.gl_renderer);
    println!(
        "reference: {} ({} steps)",
        report.reference, report.runtime_steps
    );
    println!(
        "pass-through: depth-masked={} other-dimension={} max byte diff={}",
        report.pass_through.depth_masked_pixels_unchanged,
        report.pass_through.other_dimension_pixels_unchanged,
        report.pass_through.max_byte_difference
    );
    println!(
        "{:>8} {:>10} {:>10} {:>10} {:>9} {:>9}",
        "sun", "P50", "P95", "P99", "LUT ms", "comp ms"
    );
    for scenario in &report.scenarios {
        let metrics = &scenario.linear_radiance_difference;
        println!(
            "{:>7.1}° {:>9.3}% {:>9.3}% {:>9.3}% {:>9.3} {:>9.3}",
            scenario.sun_elevation_deg,
            metrics.p50 * 100.0,
            metrics.p95 * 100.0,
            metrics.p99 * 100.0,
            scenario.sky_view_build_ms,
            scenario.composite_ms
        );
        if let Some(ratio) = scenario.solar_disk_ratio {
            println!("         solar disk radiance ratio {ratio:.4}");
        }
    }
    println!("report: {}", report_path.display());

    let failed = !report.pass_through.depth_masked_pixels_unchanged
        || !report.pass_through.other_dimension_pixels_unchanged;
    if failed {
        return Err("pack composite did not preserve non-sky pixels".into());
    }
    Ok(())
}
