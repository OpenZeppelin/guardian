use crate::cleanup_manifest::CleanupStatus;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchmarkRunReport {
    pub run_id: String,
    pub profile_name: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub measurement_seconds: f64,
    pub guardian_endpoint: String,
    pub deployment_shape: Option<String>,
    pub scheme_distribution: SchemeDistributionReport,
    pub operations: Vec<OperationReport>,
    pub capacity_estimate: CapacityEstimate,
    pub cleanup: CleanupReport,
    pub artifacts: ArtifactReport,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SchemeDistributionReport {
    pub falcon_percent: u8,
    pub ecdsa_percent: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationReport {
    pub operation: String,
    pub scope: String,
    pub attempted: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub throughput_ops_per_sec: f64,
    pub latency_ms: LatencyReport,
    #[serde(default)]
    pub failure_breakdown: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LatencyReport {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapacityEstimate {
    pub target_push_tps: f64,
    pub sustained_push_tps: f64,
    pub headroom_percent: f64,
    pub required_instances: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CleanupReport {
    pub manifest_path: String,
    pub status: CleanupStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactReport {
    pub summary_markdown: String,
    pub report_json: String,
    pub canonicalization_samples: Option<String>,
}

impl BenchmarkRunReport {
    pub fn write_to_path(&self, path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }
}
