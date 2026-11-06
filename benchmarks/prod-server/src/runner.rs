use crate::cleanup_manifest::CleanupAccountRecord;
use crate::config::{CanonicalizationConfig, Credential, ReadMode, RoleConfig, RunConfig};
use crate::error_classification::classify;
use crate::model::{AuthScheme, CanonicalizationOutcome, CanonicalizationSample};
use crate::operations::{OperationKind, create_delta_summary, proposal_payload};
use crate::report::{LatencyReport, OperationReport};
use crate::seed::SeededUser;
use crate::workload::{
    Role, canonicalization_sample_decision, operation_for_index, reader_fetches_state,
    role_for_user, warmup_operation,
};
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
    /// `None` for a reader polling a writer's account: the writer cleans it up.
    account: Option<CleanupAccountRecord>,
    metrics: BTreeMap<OperationKind, OperationAccumulator>,
    measurement_seconds: f64,
    canonicalization_samples: Vec<CanonicalizationSample>,
    wallet_signed_operations: u64,
}

pub struct RunOutput {
    pub operations: Vec<OperationReport>,
    pub cleanup_accounts: Vec<CleanupAccountRecord>,
    pub measurement_seconds: f64,
    pub canonicalization_samples: Vec<CanonicalizationSample>,
    /// Measured operations of a session run sent while the user held no
    /// session, so the wallet signed them; a write counts once.
    pub wallet_signed_operations: u64,
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
    let mut canonicalization_samples = Vec::new();
    let mut wallet_signed_operations = 0;

    while let Some(joined) = workers.join_next().await {
        let worker = joined.map_err(|error| anyhow!("worker task failed: {error}"))??;
        wallet_signed_operations += worker.wallet_signed_operations;
        cleanup_accounts.extend(worker.account);
        actual_measurement_seconds = actual_measurement_seconds.max(worker.measurement_seconds);
        canonicalization_samples.extend(worker.canonicalization_samples);
        for (operation, accumulator) in worker.metrics {
            per_scheme
                .entry((worker.scheme, operation))
                .or_default()
                .merge(accumulator);
        }
    }

    let mut operations = Vec::new();
    for operation in [
        OperationKind::GetCanonicalNonce,
        OperationKind::GetState,
        OperationKind::PushDelta,
    ] {
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
        if operation == OperationKind::GetCanonicalNonce && combined.attempted == 0 {
            continue;
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
        canonicalization_samples,
        wallet_signed_operations,
    })
}

async fn run_worker(
    config: RunConfig,
    mut user: SeededUser,
    warmup_deadline: Instant,
    end_deadline: Instant,
) -> Result<WorkerResult> {
    if let Some(roles) = config.roles.clone() {
        return run_role_worker(config, roles, user, warmup_deadline, end_deadline).await;
    }
    let mut metrics = BTreeMap::new();
    let mut measured_op_index = 0_u64;
    let mut worker_measurement_seconds = 0.0_f64;
    let mut canonicalization_samples = Vec::new();
    let mut wallet_signed_operations = 0;

    while Instant::now() < end_deadline {
        let measuring = Instant::now() >= warmup_deadline;
        let without_session = lost_session(&config, &user);
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
            OperationKind::GetCanonicalNonce => user
                .client
                .get_canonical_nonce(&user.account_id)
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
            wallet_signed_operations += u64::from(without_session);
            worker_measurement_seconds = worker_measurement_seconds.max(
                finished_at
                    .saturating_duration_since(warmup_deadline)
                    .as_secs_f64(),
            );
        }

        let sample_push = operation == OperationKind::PushDelta
            && result.is_ok()
            && canonicalization_sample_decision(
                config.canonicalization.sample_rate,
                rand::random::<f64>(),
            );
        if sample_push && let Some(nonce) = user.created_delta_nonces.last().copied() {
            canonicalization_samples
                .push(observe_canonicalization(&mut user, nonce, &config.canonicalization).await);
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
        account: cleanup_record(user),
        metrics,
        measurement_seconds: worker_measurement_seconds,
        canonicalization_samples,
        wallet_signed_operations,
    })
}

async fn run_role_worker(
    config: RunConfig,
    roles: RoleConfig,
    mut user: SeededUser,
    warmup_deadline: Instant,
    end_deadline: Instant,
) -> Result<WorkerResult> {
    let role = role_for_user(&roles, user.user_id);
    let mut metrics: BTreeMap<OperationKind, OperationAccumulator> = BTreeMap::new();
    let mut canonicalization_samples = Vec::new();
    let mut wallet_signed_operations = 0;
    let interval_ms = match role {
        Role::Reader => roles.reader_poll_interval_ms,
        Role::Writer => roles.writer_push_interval_ms,
        Role::Idle => 0,
    };

    if role != Role::Idle {
        if interval_ms > 0 {
            let jitter = rand::random::<u64>() % interval_ms;
            tokio::time::sleep(Duration::from_millis(jitter)).await;
        }
        let mut ticker = (interval_ms > 0).then(|| {
            let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker
        });
        let mut last_nonce = None;

        while Instant::now() < end_deadline {
            if let Some(ticker) = ticker.as_mut() {
                ticker.tick().await;
                if Instant::now() >= end_deadline {
                    break;
                }
            }
            let without_session = lost_session(&config, &user);
            let attempted_before = attempted(&metrics);
            match role {
                Role::Reader => {
                    read_once(
                        &mut user,
                        roles.read_mode,
                        &mut last_nonce,
                        |op, started, result| {
                            record_operation(&mut metrics, warmup_deadline, op, started, result)
                        },
                    )
                    .await;
                }
                Role::Writer => {
                    let started = Instant::now();
                    let result = push_delta(&mut user).await;
                    record_operation(
                        &mut metrics,
                        warmup_deadline,
                        OperationKind::PushDelta,
                        started,
                        &result,
                    );
                    let sample_push = result.is_ok()
                        && canonicalization_sample_decision(
                            config.canonicalization.sample_rate,
                            rand::random::<f64>(),
                        );
                    if sample_push && let Some(nonce) = user.created_delta_nonces.last().copied() {
                        canonicalization_samples.push(
                            observe_canonicalization(&mut user, nonce, &config.canonicalization)
                                .await,
                        );
                    }
                }
                Role::Idle => break,
            }
            if without_session {
                wallet_signed_operations += attempted(&metrics) - attempted_before;
            }
        }
    }

    Ok(WorkerResult {
        scheme: user.auth_scheme,
        account: cleanup_record(user),
        metrics,
        measurement_seconds: end_deadline
            .saturating_duration_since(warmup_deadline)
            .as_secs_f64(),
        canonicalization_samples,
        wallet_signed_operations,
    })
}

/// Whether a session run's user has lost its session: the client then signs
/// with the wallet, which the report must not count as session traffic.
fn lost_session(config: &RunConfig, user: &SeededUser) -> bool {
    matches!(config.credential, Credential::Session { .. }) && user.client.session().is_none()
}

fn attempted(metrics: &BTreeMap<OperationKind, OperationAccumulator>) -> u64 {
    metrics
        .values()
        .map(|accumulator| accumulator.attempted)
        .sum()
}

fn cleanup_record(user: SeededUser) -> Option<CleanupAccountRecord> {
    user.watches.is_none().then(|| CleanupAccountRecord {
        account_id: user.account_id.to_string(),
        owner_user_id: user.user_id,
        auth_scheme: user.auth_scheme.as_str().to_string(),
        created_delta_nonces: user.created_delta_nonces,
        last_known_commitment: Some(user.commitment),
    })
}

async fn read_once(
    user: &mut SeededUser,
    mode: ReadMode,
    last_nonce: &mut Option<u64>,
    mut record: impl FnMut(OperationKind, Instant, &Result<()>),
) {
    let fetch_state = match mode {
        ReadMode::FullState => true,
        ReadMode::NonceThenStateOnChange => {
            let started = Instant::now();
            let nonce = user
                .client
                .get_canonical_nonce(&user.account_id)
                .await
                .map(|response| response.nonce)
                .map_err(Error::from);
            record(
                OperationKind::GetCanonicalNonce,
                started,
                &nonce
                    .as_ref()
                    .map(|_| ())
                    .map_err(|error| anyhow!("{error:#}")),
            );
            match nonce {
                Ok(nonce) if reader_fetches_state(mode, *last_nonce, nonce) => {
                    *last_nonce = Some(nonce);
                    true
                }
                _ => false,
            }
        }
    };
    if fetch_state {
        let started = Instant::now();
        let result = user
            .client
            .get_state(&user.account_id)
            .await
            .map(|_| ())
            .map_err(Error::from);
        if result.is_err() && mode == ReadMode::NonceThenStateOnChange {
            *last_nonce = None;
        }
        record(OperationKind::GetState, started, &result);
    }
}

fn record_operation(
    metrics: &mut BTreeMap<OperationKind, OperationAccumulator>,
    warmup_deadline: Instant,
    operation: OperationKind,
    started: Instant,
    result: &Result<()>,
) {
    if started < warmup_deadline {
        return;
    }
    metrics.entry(operation).or_default().record(
        result.is_ok(),
        started.elapsed().as_secs_f64() * 1_000.0,
        result.as_ref().err(),
    );
}

async fn observe_canonicalization(
    user: &mut SeededUser,
    nonce: u64,
    config: &CanonicalizationConfig,
) -> CanonicalizationSample {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(config.timeout_seconds);
    let poll_interval = Duration::from_millis(config.poll_interval_ms);
    let mut polls = 0_u64;
    let mut last_error: Option<String>;

    // Transient poll errors never end the observation early: a delta may
    // still reach a terminal status within the deadline. The sample is
    // ObservationFailed only when the polling itself was failing at the
    // deadline; TimedOut means polls succeeded but no terminal status came.
    let (outcome, observation_error) = loop {
        polls += 1;
        match user.client.get_delta(&user.account_id, nonce).await {
            Ok(response) => {
                match response.delta {
                    Some(delta) if delta.canonical_at.is_some() => {
                        break (CanonicalizationOutcome::Canonical, None);
                    }
                    Some(delta) if delta.discarded_at.is_some() => {
                        break (CanonicalizationOutcome::Discarded, None);
                    }
                    _ => {}
                }
                last_error = None;
            }
            Err(error) => {
                last_error = Some(error.to_string());
            }
        }
        if Instant::now() + poll_interval >= deadline {
            break match last_error.take() {
                Some(error) => (CanonicalizationOutcome::ObservationFailed, Some(error)),
                None => (CanonicalizationOutcome::TimedOut, None),
            };
        }
        tokio::time::sleep(poll_interval).await;
    };

    CanonicalizationSample {
        account_id: user.account_id.to_string(),
        auth_scheme: user.auth_scheme,
        nonce,
        outcome,
        wait_ms: started.elapsed().as_secs_f64() * 1_000.0,
        polls,
        observation_error,
    }
}

/// One write as a multisig client makes it: store the proposal, approve it
/// with the user's key (the threshold is one), then push the delta. A write
/// retried after a failed push finds its proposal stored and approved
/// already, and goes on to the push.
async fn push_delta(user: &mut SeededUser) -> Result<()> {
    let nonce = user.next_nonce;
    let prev_commitment = user.commitment.clone();
    let summary = create_delta_summary(&user.account_id, nonce)?;
    let proposal_id = user
        .client
        .push_delta_proposal(&user.account_id, nonce, proposal_payload(&summary))
        .await?
        .commitment;
    let scheme = match user.auth_scheme {
        AuthScheme::Falcon => guardian_shared::SignatureScheme::Falcon,
        AuthScheme::Ecdsa => guardian_shared::SignatureScheme::Ecdsa,
    };
    let approval = guardian_shared::ProposalSignature::from_scheme(
        scheme,
        user.signer.sign_word_hex(summary.to_commitment()),
        Some(user.signer_pubkey.clone()),
    );
    match user
        .client
        .sign_delta_proposal(&user.account_id, proposal_id, approval)
        .await
    {
        Ok(_) => {}
        Err(error) if error.guardian_code().as_deref() == Some("proposal_already_signed") => {}
        Err(error) => return Err(error.into()),
    }
    let delta_payload = guardian_client::ToJson::to_json(&summary);
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
    accumulator: OperationAccumulator,
    measurement_secs: f64,
) -> OperationReport {
    OperationReport {
        operation: operation.as_str().to_string(),
        scope: scope.to_string(),
        attempted: accumulator.attempted,
        succeeded: accumulator.succeeded,
        failed: accumulator.failed,
        throughput_ops_per_sec: accumulator.succeeded as f64 / measurement_secs,
        latency_ms: LatencyReport::from_unsorted_ms(accumulator.latencies_ms),
        failure_breakdown: accumulator.failure_breakdown,
    }
}

fn _classify_client_error(error: &ClientError) -> String {
    match error {
        ClientError::ServerError(message) => message.to_ascii_lowercase(),
        other => other.to_string(),
    }
}
