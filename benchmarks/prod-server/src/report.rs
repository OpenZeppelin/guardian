use crate::cleanup_manifest::CleanupStatus;
use crate::model::{CanonicalizationOutcome, CanonicalizationSample};
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
    pub canonicalization: Option<CanonicalizationReport>,
    pub pacing: Option<PacingReport>,
    pub capacity_estimate: Option<CapacityEstimate>,
    pub cleanup: CleanupReport,
    pub artifacts: ArtifactReport,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CanonicalizationReport {
    pub sampled: u64,
    pub canonical: u64,
    pub discarded: u64,
    pub timed_out: u64,
    pub observation_failed: u64,
    pub timeout_seconds: u64,
    pub wait_ms: LatencyReport,
}

impl CanonicalizationReport {
    pub fn from_samples(samples: &[CanonicalizationSample], timeout_seconds: u64) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let count_of = |outcome: CanonicalizationOutcome| {
            samples
                .iter()
                .filter(|sample| sample.outcome == outcome)
                .count() as u64
        };
        let canonical_waits_ms = samples
            .iter()
            .filter(|sample| sample.outcome == CanonicalizationOutcome::Canonical)
            .map(|sample| sample.wait_ms)
            .collect();
        Some(Self {
            sampled: samples.len() as u64,
            canonical: count_of(CanonicalizationOutcome::Canonical),
            discarded: count_of(CanonicalizationOutcome::Discarded),
            timed_out: count_of(CanonicalizationOutcome::TimedOut),
            observation_failed: count_of(CanonicalizationOutcome::ObservationFailed),
            timeout_seconds,
            wait_ms: LatencyReport::from_unsorted_ms(canonical_waits_ms),
        })
    }
}

/// Evidence that a paced run offered the rate it declared.
///
/// `offered_rate_per_sec` is what the generator actually produced; compare it
/// against `declared_rate_per_sec`. A gap means the generator fell behind, so
/// the server was never asked for the target load and the leg understates it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PacingReport {
    pub read_interval_ms: u64,
    pub users: u32,
    pub declared_rate_per_sec: f64,
    pub offered_rate_per_sec: f64,
    pub scheduled_ticks: u64,
    pub slipped_ticks: u64,
    pub slipped_percent: f64,
    pub held_declared_rate: bool,
}

impl PacingReport {
    /// A leg that slips more than this fraction of its ticks did not offer the
    /// rate under test, so its numbers describe the generator.
    const SLIP_TOLERANCE_PERCENT: f64 = 1.0;

    pub fn new(
        read_interval_ms: u64,
        users: u32,
        scheduled_ticks: u64,
        slipped_ticks: u64,
        measurement_seconds: f64,
    ) -> Self {
        let declared_rate_per_sec = if read_interval_ms == 0 {
            0.0
        } else {
            f64::from(users) / (read_interval_ms as f64 / 1_000.0)
        };
        let slipped_percent = if scheduled_ticks == 0 {
            0.0
        } else {
            (slipped_ticks as f64 / scheduled_ticks as f64) * 100.0
        };
        Self {
            read_interval_ms,
            users,
            declared_rate_per_sec,
            offered_rate_per_sec: scheduled_ticks as f64 / measurement_seconds.max(0.001),
            scheduled_ticks,
            slipped_ticks,
            slipped_percent,
            held_declared_rate: slipped_percent <= Self::SLIP_TOLERANCE_PERCENT,
        }
    }
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

impl LatencyReport {
    pub fn from_unsorted_ms(mut values_ms: Vec<f64>) -> Self {
        values_ms.sort_by(f64::total_cmp);
        Self {
            p50: percentile(&values_ms, 0.50),
            p95: percentile(&values_ms, 0.95),
            p99: percentile(&values_ms, 0.99),
            max: values_ms.last().copied().unwrap_or(0.0),
        }
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() as f64 - 1.0) * percentile).round() as usize;
    sorted[index.min(sorted.len() - 1)]
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
