use anyhow::Result;
use chrono::Utc;
use rand::random;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct ArtifactPaths {
    pub run_dir: PathBuf,
    pub aws_dir: PathBuf,
    pub report_json: PathBuf,
    pub summary_markdown: PathBuf,
    pub cleanup_manifest: PathBuf,
    pub canonicalization_samples: PathBuf,
}

pub fn generate_run_id() -> String {
    format!(
        "{}-{:08x}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        random::<u32>()
    )
}

pub fn prepare_artifacts(base_dir: &Path, run_id: &str) -> Result<ArtifactPaths> {
    let run_dir = base_dir.join(run_id);
    let aws_dir = run_dir.join("aws");
    fs::create_dir_all(&aws_dir)?;
    Ok(ArtifactPaths {
        run_dir: run_dir.clone(),
        aws_dir,
        report_json: run_dir.join("run-report.json"),
        summary_markdown: run_dir.join("summary.md"),
        cleanup_manifest: run_dir.join("cleanup-manifest.json"),
        canonicalization_samples: run_dir.join("canonicalization-samples.json"),
    })
}
