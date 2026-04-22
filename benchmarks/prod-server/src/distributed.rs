use crate::cleanup_manifest::CleanupAccountRecord;
use crate::report::{LatencyReport, OperationReport};
use anyhow::{Result, anyhow, bail};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub const WORKER_ARTIFACT_PREFIX: &str = "BENCH_WORKER_ARTIFACT_BASE64=";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionShard {
    pub index: u32,
    pub count: u32,
}

impl ExecutionShard {
    pub fn new(index: u32, count: u32) -> Result<Self> {
        if count == 0 {
            bail!("shard_count must be greater than 0");
        }
        if index >= count {
            bail!("shard_index must be less than shard_count");
        }
        Ok(Self { index, count })
    }

    pub fn assigned_user_ids(self, total_users: u32) -> Vec<u32> {
        (0..total_users)
            .filter(|user_id| user_id % self.count == self.index)
            .collect()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerArtifact {
    pub run_id: String,
    pub worker_id: String,
    pub shard_index: u32,
    pub shard_count: u32,
    pub profile_name: String,
    pub guardian_endpoint: String,
    pub deployment_shape: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub measurement_seconds: f64,
    pub operations: Vec<OperationReport>,
    pub cleanup_accounts: Vec<CleanupAccountRecord>,
}

impl WorkerArtifact {
    pub fn write_to_path(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn worker_id(shard: ExecutionShard) -> String {
        format!("shard-{}-of-{}", shard.index, shard.count)
    }

    pub fn encoded_line(&self) -> Result<String> {
        let json = serde_json::to_vec(self)?;
        Ok(format!(
            "{}{}",
            WORKER_ARTIFACT_PREFIX,
            base64::engine::general_purpose::STANDARD.encode(json)
        ))
    }
}

#[derive(Default)]
struct OperationAggregate {
    attempted: u64,
    succeeded: u64,
    failed: u64,
    weighted_p50_sum: f64,
    weighted_p95_sum: f64,
    weighted_p99_sum: f64,
    max_latency_ms: f64,
    failure_breakdown: BTreeMap<String, u64>,
}

impl OperationAggregate {
    fn merge(&mut self, report: &OperationReport) {
        self.attempted += report.attempted;
        self.succeeded += report.succeeded;
        self.failed += report.failed;
        let weight = report.succeeded as f64;
        self.weighted_p50_sum += report.latency_ms.p50 * weight;
        self.weighted_p95_sum += report.latency_ms.p95 * weight;
        self.weighted_p99_sum += report.latency_ms.p99 * weight;
        self.max_latency_ms = self.max_latency_ms.max(report.latency_ms.max);
        for (category, count) in &report.failure_breakdown {
            *self.failure_breakdown.entry(category.clone()).or_default() += count;
        }
    }

    fn into_report(
        self,
        operation: String,
        scope: String,
        measurement_secs: f64,
    ) -> OperationReport {
        let weight = self.succeeded.max(1) as f64;
        OperationReport {
            operation,
            scope,
            attempted: self.attempted,
            succeeded: self.succeeded,
            failed: self.failed,
            throughput_ops_per_sec: self.succeeded as f64 / measurement_secs.max(0.001),
            latency_ms: LatencyReport {
                p50: if self.succeeded > 0 {
                    self.weighted_p50_sum / weight
                } else {
                    0.0
                },
                p95: if self.succeeded > 0 {
                    self.weighted_p95_sum / weight
                } else {
                    0.0
                },
                p99: if self.succeeded > 0 {
                    self.weighted_p99_sum / weight
                } else {
                    0.0
                },
                max: self.max_latency_ms,
            },
            failure_breakdown: self.failure_breakdown,
        }
    }
}

pub fn merge_worker_operations(
    worker_artifacts: &[WorkerArtifact],
    measurement_secs: f64,
) -> Result<Vec<OperationReport>> {
    if worker_artifacts.is_empty() {
        bail!("at least one worker artifact is required");
    }

    let mut aggregates = BTreeMap::<(String, String), OperationAggregate>::new();
    for artifact in worker_artifacts {
        for report in &artifact.operations {
            aggregates
                .entry((report.operation.clone(), report.scope.clone()))
                .or_default()
                .merge(report);
        }
    }

    let mut operations = Vec::with_capacity(aggregates.len());
    for ((operation, scope), aggregate) in aggregates {
        operations.push(aggregate.into_report(operation, scope, measurement_secs));
    }
    Ok(operations)
}

pub fn load_worker_artifacts(paths: &[std::path::PathBuf]) -> Result<Vec<WorkerArtifact>> {
    if paths.is_empty() {
        bail!("at least one --worker-artifact path is required");
    }

    let mut artifacts = Vec::with_capacity(paths.len());
    for path in paths {
        let artifact = WorkerArtifact::load_from_path(path).map_err(|error| {
            anyhow!("failed to load worker artifact {}: {error}", path.display())
        })?;
        artifacts.push(artifact);
    }
    Ok(artifacts)
}
