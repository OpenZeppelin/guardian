mod config;
mod report;
mod scenarios;

use anyhow::Result;
use clap::Parser;
use config::RunConfig;
use report::{RunReport, write_report};
use scenarios::{run_scenario, seed_accounts};
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<()> {
    let config = RunConfig::parse();
    config.validate()?;

    let seed_start = Instant::now();
    let users = seed_accounts(&config).await?;
    let seed_duration_ms = seed_start.elapsed().as_secs_f64() * 1_000.0;

    let metrics = run_scenario(&config, users).await?;

    println!("scenario={:?}", config.scenario);
    println!("seed_duration_ms={:.2}", seed_duration_ms);
    println!("run_duration_ms={:.2}", metrics.run_duration_ms);
    println!("total_ops={}", metrics.total_ops);
    println!("success_ops={}", metrics.success_ops);
    println!("failed_ops={}", metrics.failed_ops);
    println!("ops_per_sec={:.2}", metrics.ops_per_sec);
    println!("success_ops_per_sec={:.2}", metrics.success_ops_per_sec);
    println!("p50_ms={:.2}", metrics.latency.p50_ms);
    println!("p95_ms={:.2}", metrics.latency.p95_ms);
    println!("p99_ms={:.2}", metrics.latency.p99_ms);
    println!("max_ms={:.2}", metrics.latency.max_ms);

    if !metrics.error_samples.is_empty() {
        println!("error_samples={}", metrics.error_samples.join(" | "));
    }

    if let Some(output_path) = config.output.clone() {
        let report = RunReport {
            config,
            seed_duration_ms,
            metrics,
        };
        write_report(&output_path, &report)?;
        println!("report={}", output_path.display());
    }

    Ok(())
}
