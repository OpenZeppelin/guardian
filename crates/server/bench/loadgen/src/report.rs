use crate::config::RunConfig;
use crate::scenarios::RunMetrics;
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct RunReport {
    pub config: RunConfig,
    pub seed_duration_ms: f64,
    pub metrics: RunMetrics,
}

pub fn write_report(path: &Path, report: &RunReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create report directory: {}", parent.display()))?;
    }

    let content = serde_json::to_string_pretty(report).context("failed to serialize report")?;
    std::fs::write(path, content)
        .with_context(|| format!("failed to write report file: {}", path.display()))?;

    Ok(())
}
