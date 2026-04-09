use crate::cleanup_manifest::CleanupAccountRecord;
use crate::config::RunConfig;
use crate::error_classification::classify;
use crate::model::AuthScheme;
use crate::operations::{OperationKind, create_delta_payload};
use crate::report::{LatencyReport, OperationReport};
use crate::seed::SeededUser;
use crate::workload::{operation_for_index, warmup_operation};
use anyhow::{Error, Result, anyhow};
use guardian_client::ClientError;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use tokio::task::JoinSet;

#[derive(Default)]
struct OperationAccumulator {
    attempted: u64,
    succeeded: u64,
    failed: u64,
    latencies_ms: Vec<f64>,
    failure_breakdown: BTreeMap<String, u64>,
}

impl OperationAccumulator {
    fn record(&mut self, success: bool, latency_ms: f64, error: Option<&Error>) {
        self.attempted += 1;
        if success {
            self.succeeded += 1;
            self.latencies_ms.push(latency_ms);
        } else {
            self.failed += 1;
            let category = error.map(classify).unwrap_or_else(|| "server".to_string());
            *self.failure_breakdown.entry(category).or_default() += 1;
        }
    }

    fn merge(&mut self, other: Self) {
        self.attempted += other.attempted;
        self.succeeded += other.succeeded;
        self.failed += other.failed;
        self.latencies_ms.extend(other.latencies_ms);
        for (category, count) in other.failure_breakdown {
            *self.failure_breakdown.entry(category).or_default() += count;
        }
    }
}

struct WorkerResult {
    scheme: AuthScheme,
    account: CleanupAccountRecord,
    metrics: BTreeMap<OperationKind, OperationAccumulator>,
    measurement_seconds: f64,
}

pub struct RunOutput {
    pub operations: Vec<OperationReport>,
    pub cleanup_accounts: Vec<CleanupAccountRecord>,
    pub measurement_seconds: f64,
}

pub async fn execute(config: &RunConfig, users: Vec<SeededUser>) -> Result<RunOutput> {
    let start = Instant::now();
    let warmup_duration = Duration::from_secs(config.warmup_seconds);
    let total_duration = Duration::from_secs(config.duration_seconds);
    let warmup_deadline = start + warmup_duration;
    let end_deadline = start + total_duration;
    let measurement_secs = total_duration
        .saturating_sub(warmup_duration)
        .as_secs_f64()
        .max(0.001);

    let mut workers = JoinSet::new();
    for user in users {
        let worker_config = config.clone();
        workers.spawn(async move {
            run_worker(worker_config, user, warmup_deadline, end_deadline).await
        });
    }

    let mut per_scheme: BTreeMap<(AuthScheme, OperationKind), OperationAccumulator> =
        BTreeMap::new();
    let mut cleanup_accounts = Vec::new();
    let mut actual_measurement_seconds = 0.0_f64;

    while let Some(joined) = workers.join_next().await {
        let worker = joined.map_err(|error| anyhow!("worker task failed: {error}"))??;
        cleanup_accounts.push(worker.account);
        actual_measurement_seconds = actual_measurement_seconds.max(worker.measurement_seconds);
        for (operation, accumulator) in worker.metrics {
            per_scheme
                .entry((worker.scheme, operation))
                .or_default()
                .merge(accumulator);
        }
    }

    let mut operations = Vec::new();
    for operation in [OperationKind::GetState, OperationKind::PushDelta] {
        let mut combined = OperationAccumulator::default();
        for scheme in [AuthScheme::Falcon, AuthScheme::Ecdsa] {
            if let Some(accumulator) = per_scheme.remove(&(scheme, operation)) {
                combined.merge(accumulator_for_report(
                    &mut operations,
                    operation,
                    scheme.as_str(),
                    accumulator,
                    measurement_secs,
                ));
            }
        }
        operations.push(build_operation_report(
            operation,
            "all",
            combined,
            measurement_secs,
        ));
    }

    Ok(RunOutput {
        operations,
        cleanup_accounts,
        measurement_seconds: actual_measurement_seconds.max(0.001),
    })
}

async fn run_worker(
    config: RunConfig,
    mut user: SeededUser,
    warmup_deadline: Instant,
    end_deadline: Instant,
) -> Result<WorkerResult> {
    let mut metrics = BTreeMap::new();
    let mut measured_op_index = 0_u64;
    let mut worker_measurement_seconds = 0.0_f64;

    while Instant::now() < end_deadline {
        let measuring = Instant::now() >= warmup_deadline;
        let operation = if measuring {
            operation_for_index(config.operation_mix.reads_per_push, measured_op_index)
        } else {
            warmup_operation()
        };
        let started = Instant::now();
        let result = match operation {
            OperationKind::GetState => user
                .client
                .get_state(&user.account_id)
                .await
                .map(|_| ())
                .map_err(Into::into),
            OperationKind::PushDelta => push_delta(&mut user).await,
        };
        let finished_at = Instant::now();
        let latency_ms = started.elapsed().as_secs_f64() * 1_000.0;
        if started >= warmup_deadline {
            metrics
                .entry(operation)
                .or_insert_with(OperationAccumulator::default)
                .record(result.is_ok(), latency_ms, result.as_ref().err());
            measured_op_index += 1;
            worker_measurement_seconds = worker_measurement_seconds.max(
                finished_at
                    .saturating_duration_since(warmup_deadline)
                    .as_secs_f64(),
            );
        }

        let retire_after_push = operation == OperationKind::PushDelta
            && config.operation_mix.retire_after_first_successful_push
            && result.is_ok();
        if retire_after_push {
            let measured_until = finished_at
                .saturating_duration_since(warmup_deadline)
                .as_secs_f64();
            worker_measurement_seconds = worker_measurement_seconds.max(measured_until);
            break;
        }
    }

    if Instant::now() >= end_deadline {
        worker_measurement_seconds = worker_measurement_seconds.max(
            end_deadline
                .saturating_duration_since(warmup_deadline)
                .as_secs_f64(),
        );
    }

    Ok(WorkerResult {
        scheme: user.auth_scheme,
        account: CleanupAccountRecord {
            account_id: user.account_id.to_string(),
            owner_user_id: user.user_id,
            auth_scheme: user.auth_scheme.as_str().to_string(),
            created_delta_nonces: user.created_delta_nonces,
            last_known_commitment: Some(user.commitment),
        },
        metrics,
        measurement_seconds: worker_measurement_seconds,
    })
}

async fn push_delta(user: &mut SeededUser) -> Result<()> {
    let nonce = user.next_nonce;
    let prev_commitment = user.commitment.clone();
    let delta_payload = create_delta_payload(&user.account_id, nonce)?;
    let response = user
        .client
        .push_delta(&user.account_id, nonce, prev_commitment, delta_payload)
        .await?;
    let delta = response
        .delta
        .ok_or_else(|| anyhow!("push_delta returned no delta for nonce {}", nonce))?;

    user.next_nonce += 1;
    user.created_delta_nonces.push(nonce);
    if !delta.new_commitment.is_empty() {
        user.commitment = delta.new_commitment;
    }

    Ok(())
}

fn accumulator_for_report(
    operations: &mut Vec<OperationReport>,
    operation: OperationKind,
    scope: &str,
    accumulator: OperationAccumulator,
    measurement_secs: f64,
) -> OperationAccumulator {
    operations.push(build_operation_report(
        operation,
        scope,
        OperationAccumulator {
            attempted: accumulator.attempted,
            succeeded: accumulator.succeeded,
            failed: accumulator.failed,
            latencies_ms: accumulator.latencies_ms.clone(),
            failure_breakdown: accumulator.failure_breakdown.clone(),
        },
        measurement_secs,
    ));
    accumulator
}

fn build_operation_report(
    operation: OperationKind,
    scope: &str,
    mut accumulator: OperationAccumulator,
    measurement_secs: f64,
) -> OperationReport {
    accumulator.latencies_ms.sort_by(f64::total_cmp);
    OperationReport {
        operation: operation.as_str().to_string(),
        scope: scope.to_string(),
        attempted: accumulator.attempted,
        succeeded: accumulator.succeeded,
        failed: accumulator.failed,
        throughput_ops_per_sec: accumulator.succeeded as f64 / measurement_secs,
        latency_ms: LatencyReport {
            p50: percentile(&accumulator.latencies_ms, 0.50),
            p95: percentile(&accumulator.latencies_ms, 0.95),
            p99: percentile(&accumulator.latencies_ms, 0.99),
            max: accumulator.latencies_ms.last().copied().unwrap_or(0.0),
        },
        failure_breakdown: accumulator.failure_breakdown,
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() as f64 - 1.0) * percentile).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn _classify_client_error(error: &ClientError) -> String {
    match error {
        ClientError::ServerError(message) => message.to_ascii_lowercase(),
        other => other.to_string(),
    }
}
