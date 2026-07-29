//! N-writer scale runner for the issue #317 scalability target.
//!
//! The `run` command in [`crate::runner`] measures one multisig lifecycle in
//! depth: two accounts alternate transfers sequentially, and every stage is
//! timed. That shape answers "how long does a proposal take", not "what happens
//! with N writers at once", because it is inherently a pair —
//! `clients.split_at_mut(1)`.
//!
//! This runner answers the target's write question instead. Every fixture
//! account is an independent concurrent writer arranged in a ring: writer `i`
//! sends to writer `i + 1`, wrapping. Each writer loops
//! propose -> execute -> await canonical for the measured window.
//!
//! Why the loop includes the canonical wait rather than pipelining past it: an
//! account may hold only one non-canonical delta at a time, so a writer
//! genuinely cannot start its next operation until the previous one settles.
//! The per-writer rate is therefore an *observed* property bounded by
//! canonicalization, not a rate we choose — which is exactly what issue #317's
//! "100 concurrent users transacting" describes.
//!
//! Unlike the synthetic-delta harness in `benchmarks/prod-server`, these are
//! real transactions that are proved and submitted, so the on-chain commitment
//! actually advances and deltas actually canonicalize. That is what makes the
//! accepted->canonical criterion measurable here and nowhere else.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use guardian_client::GuardianClient;
use miden_multisig_client::{AccountId, TransactionType};
use serde::Serialize;

use crate::config::ScaleConfig;
use crate::fixture::Fixture;
use crate::runner::{
    LatencyStats, elapsed_ms, ensure_ready, execute_with_retry, latency_stats, propose_with_retry,
};
use crate::runtime::{BenchClient, load_client, load_observer};

/// Why a writer's operation did not produce a canonicalization measurement.
///
/// `AuthWindow` is separated from every other failure because issue #317 makes
/// it its own acceptance criterion ("zero auth-window failures"). Folding it
/// into a generic error bucket would make that criterion unmeasurable — the
/// mistake the synthetic harness makes, where any message containing "auth"
/// lands in one category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    AuthWindow,
    Proposal,
    Execution,
    Discarded,
    CanonicalizationTimeout,
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteRecord {
    pub writer: String,
    pub receiver: String,
    pub operation: u64,
    pub started_at: DateTime<Utc>,
    pub nonce: Option<u64>,
    pub proposal_ms: Option<u64>,
    pub execution_ms: Option<u64>,
    /// Accepted -> canonical wait. The issue #317 criterion (p95 <= 30s).
    pub canonicalization_ms: Option<u64>,
    pub total_ms: u64,
    pub failure: Option<FailureKind>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ScaleSummary {
    pub writers_configured: usize,
    pub writers_started: usize,
    pub duration_seconds: u64,
    pub operations_attempted: usize,
    pub operations_succeeded: usize,
    pub operations_failed: usize,
    /// Nonzero fails the issue #317 "zero auth-window failures" criterion.
    pub auth_window_failures: usize,
    pub failures_by_kind: Vec<(String, usize)>,
    pub canonicalization: Option<LatencyStats>,
    pub proposal: Option<LatencyStats>,
    pub execution: Option<LatencyStats>,
    pub verdicts: Vec<CriterionVerdict>,
}

/// A single issue #317 acceptance criterion evaluated against this run.
///
/// `NotMeasured` is a first-class outcome, never a silent pass: a run that
/// produced no canonicalization samples has not shown the criterion holds, and
/// reporting it as passing would invert the meaning of the result.
#[derive(Debug, Serialize)]
pub struct CriterionVerdict {
    pub criterion: String,
    pub measured: Option<f64>,
    pub target: f64,
    pub verdict: String,
    pub note: String,
}

const CANONICALIZATION_P95_TARGET_MS: f64 = 30_000.0;

pub async fn run(config: &ScaleConfig) -> Result<PathBuf> {
    let fixture = Fixture::load(&config.accounts_file)?;
    let writers = config.writers.min(fixture.accounts.len());
    if writers < 2 {
        bail!(
            "need at least 2 accounts to form a transfer ring, fixture has {} and writers is {}",
            fixture.accounts.len(),
            config.writers
        );
    }
    if config.writers > fixture.accounts.len() {
        // Silently driving fewer writers than asked would overstate the
        // concurrency every downstream number is attributed to.
        bail!(
            "config requests {} writers but the fixture has {} accounts; provision more with \
             `prepare --accounts {}`",
            config.writers,
            fixture.accounts.len(),
            config.writers
        );
    }

    let run_config = config.to_run_config();
    let faucet_id = crate::runner::parse_faucet_id(&run_config)?;

    let mut handles = Vec::with_capacity(writers);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    let gate = Arc::new(StartGate::default());

    // One OS thread per writer, each with its own current-thread runtime,
    // rather than `tokio::spawn`. The multisig client builder holds a
    // non-`Send` `dyn StoreFactory` across an await, so a writer's future
    // cannot cross threads even though `MultisigClient` itself is `Send`.
    // Dedicated threads also suit the workload: proving is CPU-bound, so real
    // parallelism is what makes N writers concurrent rather than interleaved.
    for index in 0..writers {
        let fixture = fixture.clone();
        let run_config = run_config.clone();
        let receiver_index = (index + 1) % writers;
        let ready_tx = ready_tx.clone();
        let gate = Arc::clone(&gate);
        handles.push(std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("failed to build writer runtime")?;
            runtime.block_on(drive_writer(
                fixture,
                run_config,
                index,
                receiver_index,
                faucet_id,
                ready_tx,
                gate,
            ))
        }));
    }
    drop(ready_tx);

    // Wait for every writer to finish connecting and syncing before starting the
    // clock. Clients are loaded per writer and can be slow, so a clock started
    // at spawn time gives late writers a shorter window -- or none at all --
    // while attributing the results to full concurrency.
    let mut initialisation_errors = Vec::new();
    for _ in 0..writers {
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => initialisation_errors.push(format!("{error:#}")),
            Err(_) => initialisation_errors
                .push("writer thread ended before signalling readiness".to_string()),
        }
    }

    if initialisation_errors.is_empty() {
        gate.start(Instant::now() + Duration::from_secs(config.duration_seconds));
    } else {
        // Releasing the survivors would measure fewer writers than configured
        // while labelling the result with the configured count.
        gate.abort();
    }

    let mut records = Vec::new();
    let mut participated = 0usize;
    for handle in handles {
        match handle.join() {
            Ok(Ok(writer_records)) => {
                // Count writers that actually produced load, not writers that
                // merely returned: a writer that contributed no operation did
                // not take part in the measured concurrency.
                if !writer_records.is_empty() {
                    participated += 1;
                }
                records.extend(writer_records);
            }
            Ok(Err(error)) => eprintln!("writer failed: {error:#}"),
            Err(_) => eprintln!("writer thread panicked"),
        }
    }

    if !initialisation_errors.is_empty() {
        bail!(
            "{} of {} writers failed to initialise, so the run was not started: {}",
            initialisation_errors.len(),
            writers,
            initialisation_errors.join("; ")
        );
    }

    write_artifacts(config, writers, participated, records)
}

/// Holds writers at the start line until every one of them is connected.
#[derive(Default)]
pub(crate) struct StartGate {
    state: Mutex<GateState>,
    signal: Condvar,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum GateState {
    #[default]
    Waiting,
    Start(Instant),
    Abort,
}

impl StartGate {
    fn start(&self, deadline: Instant) {
        *self.state.lock().expect("start gate poisoned") = GateState::Start(deadline);
        self.signal.notify_all();
    }

    fn abort(&self) {
        *self.state.lock().expect("start gate poisoned") = GateState::Abort;
        self.signal.notify_all();
    }

    /// Block until the run starts, returning the shared deadline, or `None` if
    /// the run was abandoned during initialisation.
    fn wait(&self) -> Option<Instant> {
        let mut state = self.state.lock().expect("start gate poisoned");
        while *state == GateState::Waiting {
            state = self.signal.wait(state).expect("start gate poisoned");
        }
        match *state {
            GateState::Start(deadline) => Some(deadline),
            _ => None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn drive_writer(
    fixture: Fixture,
    run_config: crate::config::RunConfig,
    index: usize,
    receiver_index: usize,
    faucet_id: AccountId,
    ready: std::sync::mpsc::Sender<Result<()>>,
    gate: Arc<StartGate>,
) -> Result<Vec<WriteRecord>> {
    let sender_fixture = fixture.accounts[index].clone();
    let receiver_fixture = fixture.accounts[receiver_index].clone();

    // Everything that can be slow or can fail happens before readiness is
    // signalled, so the measured window covers load generation only.
    let initialised = async {
        let sender = load_client(&fixture, &sender_fixture, &run_config)
            .await
            .with_context(|| format!("failed to load client for {}", sender_fixture.label))?;
        let receiver_id = AccountId::from_hex(&receiver_fixture.account_id)
            .with_context(|| format!("invalid account ID for {}", receiver_fixture.label))?;
        let observer = load_observer(&fixture, &sender_fixture).await?;
        Ok::<_, anyhow::Error>((sender, receiver_id, observer))
    }
    .await;

    let (mut sender, receiver_id, mut observer) = match initialised {
        Ok(parts) => {
            let _ = ready.send(Ok(()));
            parts
        }
        Err(error) => {
            let message = format!("{error:#}");
            let _ = ready.send(Err(anyhow::anyhow!(message)));
            return Err(error);
        }
    };

    let Some(deadline) = gate.wait() else {
        // Another writer failed to initialise; the run was abandoned.
        return Ok(Vec::new());
    };

    let mut records = Vec::new();
    let mut operation = 0u64;
    while Instant::now() < deadline {
        operation += 1;
        records.push(
            execute_write(
                &mut sender,
                &mut observer,
                receiver_id,
                &receiver_fixture.label,
                faucet_id,
                &run_config,
                operation,
            )
            .await,
        );
    }
    Ok(records)
}

/// One propose -> execute -> await-canonical cycle.
///
/// Returns a record rather than an error: a failed operation is data the run
/// needs (especially an auth-window failure, which is itself a criterion), not
/// a reason to abandon the writer and lose the rest of the window.
async fn execute_write(
    sender: &mut BenchClient,
    observer: &mut GuardianClient,
    receiver_id: AccountId,
    receiver_label: &str,
    faucet_id: AccountId,
    run_config: &crate::config::RunConfig,
    operation: u64,
) -> WriteRecord {
    let started_at = Utc::now();
    let started = Instant::now();
    let mut record = WriteRecord {
        writer: sender.label.clone(),
        receiver: receiver_label.to_string(),
        operation,
        started_at,
        nonce: None,
        proposal_ms: None,
        execution_ms: None,
        canonicalization_ms: None,
        total_ms: 0,
        failure: None,
        error: None,
    };

    let proposal_started = Instant::now();
    let attempt = match propose_with_retry(
        sender,
        TransactionType::transfer(receiver_id, faucet_id, run_config.amount),
        run_config,
    )
    .await
    {
        Ok(attempt) => attempt,
        Err(error) => {
            return fail(
                record,
                started,
                classify(&error, FailureKind::Proposal),
                error,
            );
        }
    };
    record.proposal_ms = Some(elapsed_ms(proposal_started));

    let proposal = attempt.proposal;
    if let Err(error) = ensure_ready(&proposal.status, &proposal.id) {
        return fail(record, started, FailureKind::Proposal, error);
    }
    record.nonce = Some(proposal.nonce);

    let execution_started = Instant::now();
    if let Err(error) = execute_with_retry(sender, &proposal.id, run_config).await {
        return fail(
            record,
            started,
            classify(&error, FailureKind::Execution),
            error,
        );
    }
    record.execution_ms = Some(elapsed_ms(execution_started));

    // The delta is accepted from here; this wait is the #317 criterion.
    let canonicalization_started = Instant::now();
    match await_canonical_status(
        observer,
        sender.account_id,
        proposal.nonce,
        run_config,
        canonicalization_started,
    )
    .await
    {
        Ok(wait_ms) => {
            record.canonicalization_ms = Some(wait_ms);
            record.total_ms = elapsed_ms(started);
            record
        }
        Err((kind, error)) => fail(record, started, kind, error),
    }
}

/// Poll Guardian until the delta reaches a terminal state.
///
/// Distinguishes discard from timeout: a discarded delta means Guardian
/// rejected it, while a timeout means it never settled. Collapsing the two
/// would hide a correctness signal inside a latency one.
async fn await_canonical_status(
    observer: &mut GuardianClient,
    account_id: AccountId,
    nonce: u64,
    run_config: &crate::config::RunConfig,
    started: Instant,
) -> Result<u64, (FailureKind, anyhow::Error)> {
    let deadline = started + Duration::from_secs(run_config.timeout_seconds);
    loop {
        match observer.get_delta(&account_id, nonce).await {
            Ok(response) => {
                if let Some(delta) = response.delta {
                    if delta.canonical_at.is_some() {
                        return Ok(elapsed_ms(started));
                    }
                    if delta.discarded_at.is_some() {
                        return Err((
                            FailureKind::Discarded,
                            anyhow::anyhow!("Guardian discarded nonce {nonce}"),
                        ));
                    }
                }
            }
            Err(error) => {
                if is_auth_window(&error.to_string()) {
                    return Err((FailureKind::AuthWindow, anyhow::anyhow!("{error}")));
                }
            }
        }
        if Instant::now() >= deadline {
            return Err((
                FailureKind::CanonicalizationTimeout,
                anyhow::anyhow!(
                    "nonce {nonce} did not canonicalize within {}s",
                    run_config.timeout_seconds
                ),
            ));
        }
        tokio::time::sleep(Duration::from_millis(run_config.poll_interval_ms)).await;
    }
}

/// The server returns auth-window expiry as a generic authentication failure —
/// both are `GuardianError::AuthenticationFailed`, differing only in message —
/// so the distinguishing substring is the only signal available without a
/// server-side contract change.
fn is_auth_window(message: &str) -> bool {
    message.contains("outside allowed window")
}

/// Classify a failure, preferring the auth-window criterion over the stage it
/// happened in: an auth-window expiry during execution is still an auth-window
/// failure, and #317 counts it as such regardless of where it surfaced.
fn classify(error: &anyhow::Error, stage: FailureKind) -> FailureKind {
    if is_auth_window(&format!("{error:#}")) {
        return FailureKind::AuthWindow;
    }
    stage
}

fn fail(
    mut record: WriteRecord,
    started: Instant,
    kind: FailureKind,
    error: anyhow::Error,
) -> WriteRecord {
    record.failure = Some(kind);
    record.error = Some(format!("{error:#}"));
    record.total_ms = elapsed_ms(started);
    record
}

fn write_artifacts(
    config: &ScaleConfig,
    writers_configured: usize,
    writers_started: usize,
    records: Vec<WriteRecord>,
) -> Result<PathBuf> {
    std::fs::create_dir_all(&config.artifacts_dir).with_context(|| {
        format!(
            "failed to create artifacts directory {}",
            config.artifacts_dir.display()
        )
    })?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let records_path = config
        .artifacts_dir
        .join(format!("scale-{stamp}-records.jsonl"));
    let summary_path = config
        .artifacts_dir
        .join(format!("scale-{stamp}-summary.json"));

    let mut buffer = String::new();
    for record in &records {
        buffer.push_str(&serde_json::to_string(record)?);
        buffer.push('\n');
    }
    std::fs::write(&records_path, buffer)
        .with_context(|| format!("failed to write {}", records_path.display()))?;

    let summary = summarize(config, writers_configured, writers_started, &records);
    std::fs::write(&summary_path, serde_json::to_string_pretty(&summary)?)
        .with_context(|| format!("failed to write {}", summary_path.display()))?;

    print_summary(&summary, &records_path, &summary_path);
    Ok(summary_path)
}

pub(crate) fn summarize(
    config: &ScaleConfig,
    writers_configured: usize,
    writers_started: usize,
    records: &[WriteRecord],
) -> ScaleSummary {
    let succeeded = records
        .iter()
        .filter(|record| record.failure.is_none())
        .count();
    let auth_window_failures = records
        .iter()
        .filter(|record| record.failure == Some(FailureKind::AuthWindow))
        .count();

    let mut failures_by_kind: Vec<(String, usize)> = Vec::new();
    for record in records {
        if let Some(kind) = record.failure {
            let key = format!("{:?}", kind);
            match failures_by_kind.iter_mut().find(|(name, _)| name == &key) {
                Some((_, count)) => *count += 1,
                None => failures_by_kind.push((key, 1)),
            }
        }
    }
    failures_by_kind.sort_by(|left, right| right.1.cmp(&left.1));

    // Every ACCEPTED delta belongs in the canonicalization population, not just
    // the ones that reached canonical. Percentiles over successes alone let a
    // run pass while an arbitrary share of accepted deltas never settled: 100
    // deltas where 60 canonicalize in 1s and 40 time out would report p95 = 1s.
    //
    // Unsettled deltas are right-censored at the poll bound: their true wait is
    // at least that, so substituting the bound yields a LOWER bound on the real
    // p95. That can understate the delay but can never manufacture a pass --
    // once more than 5% are censored, p95 lands on the bound and fails on its
    // own, with no special case needed.
    let censor_bound_ms = config.timeout_seconds.saturating_mul(1_000);
    let accepted: Vec<&WriteRecord> = records
        .iter()
        .filter(|record| record.execution_ms.is_some())
        .collect();
    let canonical_samples = accepted
        .iter()
        .filter(|record| record.canonicalization_ms.is_some())
        .count();
    let censored = accepted.len() - canonical_samples;
    let canonicalization = latency_stats(
        accepted
            .iter()
            .map(|record| record.canonicalization_ms.unwrap_or(censor_bound_ms)),
    )
    .ok();
    let proposal = latency_stats(records.iter().filter_map(|r| r.proposal_ms)).ok();
    let execution = latency_stats(records.iter().filter_map(|r| r.execution_ms)).ok();

    let verdicts = vec![
        canonicalization_verdict(canonicalization.as_ref(), canonical_samples, censored),
        auth_window_verdict(auth_window_failures, records.len()),
        concurrency_verdict(writers_configured, writers_started),
    ];

    ScaleSummary {
        writers_configured,
        writers_started,
        duration_seconds: config.duration_seconds,
        operations_attempted: records.len(),
        operations_succeeded: succeeded,
        operations_failed: records.len() - succeeded,
        auth_window_failures,
        failures_by_kind,
        canonicalization,
        proposal,
        execution,
        verdicts,
    }
}

fn canonicalization_verdict(
    stats: Option<&LatencyStats>,
    canonical: usize,
    censored: usize,
) -> CriterionVerdict {
    match stats {
        Some(stats) => {
            let note = if censored == 0 {
                format!("{canonical} accepted deltas, all reached canonical")
            } else {
                // Say so explicitly: the reported p95 is a lower bound, so a
                // reader must not treat it as the settled figure.
                format!(
                    "{} accepted deltas: {canonical} canonical, {censored} unsettled and censored                      at {}ms; p95 is a lower bound",
                    canonical + censored,
                    stats.max_ms
                )
            };
            CriterionVerdict {
                criterion: "canonicalization_p95_ms".to_string(),
                measured: Some(stats.p95_ms as f64),
                target: CANONICALIZATION_P95_TARGET_MS,
                verdict: if (stats.p95_ms as f64) <= CANONICALIZATION_P95_TARGET_MS {
                    "pass".to_string()
                } else {
                    "fail".to_string()
                },
                note,
            }
        }
        // No accepted deltas at all: nothing was observed either way. Distinct
        // from accepted deltas that failed to settle, which is a real failure.
        None => CriterionVerdict {
            criterion: "canonicalization_p95_ms".to_string(),
            measured: None,
            target: CANONICALIZATION_P95_TARGET_MS,
            verdict: "not_measured".to_string(),
            note: "no delta was accepted, so canonicalization was never exercised".to_string(),
        },
    }
}

fn auth_window_verdict(failures: usize, attempted: usize) -> CriterionVerdict {
    // With no operations there is no evidence either way, so this is
    // `not_measured` rather than a zero that looks like a pass.
    if attempted == 0 {
        return CriterionVerdict {
            criterion: "auth_window_failures".to_string(),
            measured: None,
            target: 0.0,
            verdict: "not_measured".to_string(),
            note: "no operations were attempted".to_string(),
        };
    }
    CriterionVerdict {
        criterion: "auth_window_failures".to_string(),
        measured: Some(failures as f64),
        target: 0.0,
        verdict: if failures == 0 {
            "pass".to_string()
        } else {
            "fail".to_string()
        },
        note: format!("over {attempted} operations"),
    }
}

fn concurrency_verdict(configured: usize, started: usize) -> CriterionVerdict {
    CriterionVerdict {
        criterion: "concurrent_writers".to_string(),
        measured: Some(started as f64),
        target: configured as f64,
        verdict: if started >= configured {
            "pass".to_string()
        } else {
            "fail".to_string()
        },
        note: format!("{started} of {configured} writers produced records"),
    }
}

fn print_summary(summary: &ScaleSummary, records_path: &Path, summary_path: &Path) {
    println!(
        "writers {}/{} | operations {} ({} ok, {} failed)",
        summary.writers_started,
        summary.writers_configured,
        summary.operations_attempted,
        summary.operations_succeeded,
        summary.operations_failed
    );
    if let Some(stats) = &summary.canonicalization {
        println!(
            "canonicalization ms: p50 {} p95 {} max {} ({} samples)",
            stats.p50_ms, stats.p95_ms, stats.max_ms, stats.samples
        );
    }
    for verdict in &summary.verdicts {
        println!(
            "  [{}] {} measured={:?} target={}  {}",
            verdict.verdict, verdict.criterion, verdict.measured, verdict.target, verdict.note
        );
    }
    println!("records: {}", records_path.display());
    println!("summary: {}", summary_path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(writers: usize) -> ScaleConfig {
        ScaleConfig {
            accounts_file: PathBuf::from("accounts.json"),
            faucet_id: "0x00".to_string(),
            writers,
            duration_seconds: 60,
            amount: 1,
            poll_interval_ms: 1000,
            timeout_seconds: 180,
            proposal_retry_interval_ms: 1000,
            proposal_retry_timeout_seconds: 180,
            artifacts_dir: PathBuf::from("reports"),
        }
    }

    fn record(canonicalization_ms: Option<u64>, failure: Option<FailureKind>) -> WriteRecord {
        WriteRecord {
            writer: "alice".to_string(),
            receiver: "bob".to_string(),
            operation: 1,
            started_at: Utc::now(),
            nonce: Some(1),
            proposal_ms: Some(10),
            execution_ms: Some(20),
            canonicalization_ms,
            total_ms: 30,
            failure,
            error: None,
        }
    }

    #[test]
    fn canonicalization_passes_when_p95_is_within_target() {
        let records: Vec<_> = (0..20).map(|_| record(Some(5_000), None)).collect();
        let summary = summarize(&config(2), 2, 2, &records);

        let verdict = &summary.verdicts[0];
        assert_eq!(verdict.criterion, "canonicalization_p95_ms");
        assert_eq!(verdict.verdict, "pass");
    }

    #[test]
    fn canonicalization_fails_when_p95_exceeds_thirty_seconds() {
        let records: Vec<_> = (0..20).map(|_| record(Some(45_000), None)).collect();
        let summary = summarize(&config(2), 2, 2, &records);

        assert_eq!(summary.verdicts[0].verdict, "fail");
    }

    #[test]
    fn canonicalization_is_not_measured_when_no_delta_was_ever_accepted() {
        // Proposal failures never reach acceptance, so there is nothing to
        // measure -- as opposed to accepted deltas that fail to settle.
        let mut records: Vec<_> = (0..5)
            .map(|_| record(None, Some(FailureKind::Proposal)))
            .collect();
        for record in &mut records {
            record.execution_ms = None;
        }
        let summary = summarize(&config(2), 2, 2, &records);

        assert_eq!(summary.verdicts[0].verdict, "not_measured");
        assert!(summary.verdicts[0].measured.is_none());
    }

    #[test]
    fn accepted_deltas_that_never_settle_fail_rather_than_leaving_the_percentile() {
        // 60 fast, 40 timed out. Filtering to successes would report p95 = 1s
        // and pass; censoring the unsettled ones at the bound must fail.
        let mut records: Vec<_> = (0..60).map(|_| record(Some(1_000), None)).collect();
        records.extend((0..40).map(|_| record(None, Some(FailureKind::CanonicalizationTimeout))));
        let summary = summarize(&config(2), 2, 2, &records);

        let verdict = &summary.verdicts[0];
        assert_eq!(verdict.verdict, "fail");
        assert_eq!(verdict.measured, Some(180_000.0));
        assert!(verdict.note.contains("40 unsettled"));
    }

    #[test]
    fn discarded_deltas_also_count_against_the_criterion() {
        let mut records: Vec<_> = (0..50).map(|_| record(Some(1_000), None)).collect();
        records.extend((0..50).map(|_| record(None, Some(FailureKind::Discarded))));
        let summary = summarize(&config(2), 2, 2, &records);

        assert_eq!(summary.verdicts[0].verdict, "fail");
    }

    #[test]
    fn a_small_share_of_unsettled_deltas_still_allows_a_pass() {
        // 99 fast, 1 unsettled: p95 stays below the target, and the note must
        // flag that the figure is a lower bound.
        let mut records: Vec<_> = (0..99).map(|_| record(Some(1_000), None)).collect();
        records.push(record(None, Some(FailureKind::CanonicalizationTimeout)));
        let summary = summarize(&config(2), 2, 2, &records);

        assert_eq!(summary.verdicts[0].verdict, "pass");
        assert!(summary.verdicts[0].note.contains("lower bound"));
    }

    #[test]
    fn a_single_auth_window_failure_fails_the_criterion() {
        let mut records: Vec<_> = (0..99).map(|_| record(Some(1_000), None)).collect();
        records.push(record(None, Some(FailureKind::AuthWindow)));
        let summary = summarize(&config(2), 2, 2, &records);

        assert_eq!(summary.auth_window_failures, 1);
        assert_eq!(summary.verdicts[1].verdict, "fail");
    }

    #[test]
    fn auth_window_is_not_measured_when_no_operations_ran() {
        let summary = summarize(&config(2), 2, 0, &[]);

        assert_eq!(summary.verdicts[1].verdict, "not_measured");
    }

    #[test]
    fn concurrency_fails_when_writers_did_not_all_participate() {
        let records = vec![record(Some(1_000), None)];
        let summary = summarize(&config(8), 8, 6, &records);

        let verdict = &summary.verdicts[2];
        assert_eq!(verdict.criterion, "concurrent_writers");
        assert_eq!(verdict.verdict, "fail");
        assert_eq!(verdict.measured, Some(6.0));
    }

    #[test]
    fn start_gate_releases_every_writer_with_one_shared_deadline() {
        let gate = Arc::new(StartGate::default());
        let deadline = Instant::now() + Duration::from_secs(30);

        let waiters: Vec<_> = (0..4)
            .map(|_| {
                let gate = Arc::clone(&gate);
                std::thread::spawn(move || gate.wait())
            })
            .collect();

        gate.start(deadline);

        for waiter in waiters {
            assert_eq!(waiter.join().unwrap(), Some(deadline));
        }
    }

    #[test]
    fn start_gate_abort_releases_waiters_without_a_deadline() {
        // A writer that failed to initialise must not leave the others parked
        // on the gate forever.
        let gate = Arc::new(StartGate::default());
        let waiter = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || gate.wait())
        };

        gate.abort();

        assert_eq!(waiter.join().unwrap(), None);
    }

    #[test]
    fn start_gate_does_not_block_a_writer_that_arrives_after_the_start() {
        let gate = StartGate::default();
        let deadline = Instant::now() + Duration::from_secs(30);
        gate.start(deadline);

        assert_eq!(gate.wait(), Some(deadline));
    }

    #[test]
    fn auth_window_errors_are_recognised_from_the_server_message() {
        assert!(is_auth_window(
            "Authentication failed: Request timestamp outside allowed window: 301000ms drift (max 300000ms)"
        ));
        assert!(!is_auth_window("Authentication failed: invalid signature"));
    }

    #[test]
    fn failures_are_grouped_by_kind_most_frequent_first() {
        let mut records: Vec<_> = (0..3)
            .map(|_| record(None, Some(FailureKind::CanonicalizationTimeout)))
            .collect();
        records.push(record(None, Some(FailureKind::AuthWindow)));
        let summary = summarize(&config(2), 2, 2, &records);

        assert_eq!(summary.failures_by_kind[0].0, "CanonicalizationTimeout");
        assert_eq!(summary.failures_by_kind[0].1, 3);
    }
}
