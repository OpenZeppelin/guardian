use std::{fmt::Display, future::Future, time::Duration};

use guardian_shared::SignatureScheme;
use tokio::time::{Instant, timeout, timeout_at};
use tokio_util::sync::CancellationToken;

use crate::builder::handle::StartupRecoveryError;
use crate::coordination::Lease;
use crate::delta_object::DeltaObject;
use crate::jobs::canonicalization::spawn_lease_renewal;
use crate::metadata::{Auth, MidenNetworkType};
use crate::network::{
    AppliedState, NetworkClient, ReconstructError, RpcReadMode, StateVerification, reconstructor,
};
use crate::state::AppState;
use crate::storage::is_record_corruption;
use crate::storage::postgres::recovery::{
    PendingOutcome, ProposalChange, ProposalReview, RecoveryCandidate, RecoveryStore,
    ReviewedProposal, ScanProgress,
};

const PAGE_SIZE: i64 = 100;
const MAX_DELTAS: u32 = 128;
const LEASE_TTL: Duration = Duration::from_secs(30);
const SCAN_BUDGET: Duration = Duration::from_secs(300);
const REMAINING_COUNT_BUDGET: Duration = Duration::from_secs(1);
const SCAN_CONTROL_BUDGET: Duration = Duration::from_secs(6);
const STALE_LEASE_WAIT: Duration = Duration::from_secs(35);
const LEASE_RETRY_INTERVAL: Duration = Duration::from_secs(2);
const MAX_CONSECUTIVE_FAILED_CHECKS: usize = 3;

#[derive(Debug)]
enum RecoveryOutcome {
    Unchanged,
    Verified(Box<Restoration>),
    Retryable(String),
    Unresolved(String),
}

/// What a restore writes for one account.
#[derive(Debug)]
struct Restoration {
    state: AppliedState,
    auth: Auth,
    proposals: ProposalReview,
}

impl Restoration {
    /// The proposals a restore removes or rewrites.
    fn changed_proposals(&self) -> impl Iterator<Item = &ReviewedProposal> {
        let reviewed = match &self.proposals {
            ProposalReview::Reviewed(reviewed) => reviewed.as_slice(),
            ProposalReview::Unreadable => &[],
        };
        reviewed
            .iter()
            .filter(|proposal| proposal.change != ProposalChange::Keep)
    }

    fn log_proposal_changes(&self, account_id: &str, dry_run: bool) {
        if self.proposals == ProposalReview::Unreadable {
            tracing::warn!(
                %account_id,
                dry_run,
                "Account recovery could not read the pending proposals and leaves them as they are"
            );
        }
        for proposal in self.changed_proposals() {
            tracing::info!(
                %account_id,
                proposal = %proposal.commitment,
                change = proposal.change.label(),
                dry_run,
                "Account recovery proposal review"
            );
        }
    }
}

enum PendingRecord {
    Clear,
    Write(PendingOutcome, String),
}

/// Whether a scan writes what it finds. A dry run reads and reports only.
#[derive(Clone, Copy)]
enum ScanWrites<'a> {
    Apply(&'a Lease),
    DryRun,
}

#[derive(Debug, Default)]
struct ScanSummary {
    checked: usize,
    restored: usize,
    restorable: usize,
    unchanged: usize,
    retryable: usize,
    unresolved: i64,
    failed: usize,
    last_checked: Option<String>,
    stopped_early: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum ScanStep {
    Completed { unresolved: i64 },
    Resume(ScanProgress),
}

impl ScanSummary {
    /// What a scan that started at `started` leaves for the next startup: its position when
    /// it stopped early, and otherwise a pass from the first account. Only a pass that
    /// started at the first account, reached the last one within one startup and deferred
    /// none completes the scan.
    fn next_step(&self, started: ScanProgress) -> ScanStep {
        if self.stopped_early {
            return ScanStep::Resume(ScanProgress {
                resume_after: self.last_checked.clone().unwrap_or(started.resume_after),
            });
        }
        let covered_every_account = started.resume_after.is_empty();
        if covered_every_account && self.retryable == 0 && self.failed == 0 {
            ScanStep::Completed {
                unresolved: self.unresolved,
            }
        } else {
            ScanStep::Resume(ScanProgress::default())
        }
    }
}

pub(crate) async fn run_once(
    state: &AppState,
    store: &RecoveryStore,
    network_type: MidenNetworkType,
) -> Result<(), StartupRecoveryError> {
    run_within(state, store, network_type, SCAN_BUDGET, STALE_LEASE_WAIT).await
}

/// Takes the recovery lease, waiting out one a stopped process left behind. A lease that is
/// still held after `wait` belongs to a live process.
async fn acquire_lease(
    store: &RecoveryStore,
    wait: Duration,
) -> Result<Lease, StartupRecoveryError> {
    let give_up_at = Instant::now() + wait;
    let mut waiting = false;
    loop {
        let acquired = bounded_db_call(
            "acquire the recovery lease",
            store.leader.try_acquire(LEASE_TTL),
        )
        .await
        .map_err(StartupRecoveryError::LeaseUnavailable)?;
        match acquired {
            Some(lease) => return Ok(lease),
            None if Instant::now() + LEASE_RETRY_INTERVAL > give_up_at => {
                return Err(StartupRecoveryError::LeaseBusy);
            }
            None => {
                if !waiting {
                    tracing::info!("Waiting for the account recovery lease another process holds");
                    waiting = true;
                }
                tokio::time::sleep(LEASE_RETRY_INTERVAL).await
            }
        }
    }
}

/// Runs the scan without a lease and without writing anything: the log lines say what an
/// applying run would do with each account.
pub(crate) async fn dry_run(
    state: &AppState,
    store: &RecoveryStore,
    network_type: MidenNetworkType,
) -> Result<(), StartupRecoveryError> {
    if bounded_db_call(
        "read the recovery checkpoint",
        store.startup_completed(network_type),
    )
    .await
    .unwrap_or(false)
    {
        tracing::info!(
            ?network_type,
            "Dry run: account recovery already completed, so apply would skip the scan"
        );
    }
    run(
        state,
        store,
        network_type,
        ScanWrites::DryRun,
        &CancellationToken::new(),
        "",
        Instant::now() + SCAN_BUDGET,
    )
    .await
    .map(drop)
    .map_err(StartupRecoveryError::Incomplete)
}

async fn run_within(
    state: &AppState,
    store: &RecoveryStore,
    network_type: MidenNetworkType,
    budget: Duration,
    stale_lease_wait: Duration,
) -> Result<(), StartupRecoveryError> {
    let lease = acquire_lease(store, stale_lease_wait).await?;
    let cancel = CancellationToken::new();
    let renewal = spawn_lease_renewal(
        store.leader.clone(),
        lease.clone(),
        LEASE_TTL,
        LEASE_TTL / 3,
        cancel.clone(),
        "Account recovery",
    );
    let result = async {
        if bounded_db_call(
            "read the recovery checkpoint",
            store.startup_completed(network_type),
        )
        .await?
        {
            tracing::info!(
                ?network_type,
                "Account recovery already completed; skipping the scan"
            );
            return Ok(());
        }
        let started =
            bounded_db_call("read recovery progress", store.progress(network_type)).await?;
        let summary = run(
            state,
            store,
            network_type,
            ScanWrites::Apply(&lease),
            &cancel,
            &started.resume_after,
            Instant::now() + budget,
        )
        .await?;
        match summary.next_step(started) {
            ScanStep::Completed { unresolved } => {
                bounded_db_call(
                    "record recovery completion",
                    store.complete_startup(network_type, unresolved, &lease),
                )
                .await?;
                tracing::info!(
                    unresolved,
                    "Account recovery completed; later startups skip the scan"
                );
                Ok(())
            }
            ScanStep::Resume(progress) => {
                tracing::warn!(
                    resume_after = %progress.resume_after,
                    "Account recovery will continue on the next eligible startup"
                );
                bounded_db_call(
                    "save recovery progress",
                    store.save_progress(network_type, &progress, &lease),
                )
                .await
            }
        }
    }
    .await;
    let held = bounded_db_call(
        "confirm the recovery lease",
        store.leader.verify_held(&lease),
    )
    .await;
    renewal.abort();
    let _ = renewal.await;
    let released = bounded_db_call("release the recovery lease", store.leader.release(lease)).await;
    if !held.map_err(StartupRecoveryError::LeaseUnavailable)? {
        return Err(StartupRecoveryError::LeaseUnavailable(
            "The recovery lease was lost".into(),
        ));
    }
    result
        .and(released)
        .map_err(StartupRecoveryError::Incomplete)
}

async fn bounded_db_call<T, E: Display>(
    operation: &str,
    work: impl Future<Output = Result<T, E>>,
) -> Result<T, String> {
    timeout(SCAN_CONTROL_BUDGET, work)
        .await
        .map_err(|_| format!("Timed out while trying to {operation}"))?
        .map_err(|error| error.to_string())
}

async fn run(
    state: &AppState,
    store: &RecoveryStore,
    network_type: MidenNetworkType,
    writes: ScanWrites<'_>,
    cancel: &CancellationToken,
    resume_after: &str,
    deadline: Instant,
) -> Result<ScanSummary, String> {
    let dry_run = matches!(writes, ScanWrites::DryRun);
    tracing::info!(?network_type, %resume_after, dry_run, "Starting account recovery scan");
    let mut after = resume_after.to_owned();
    let mut summary = ScanSummary::default();
    let mut consecutive_failed_checks = 0;
    let mut position_before_failed_checks = None;
    'scan: loop {
        let candidates =
            match timeout_at(deadline, store.candidates(network_type, &after, PAGE_SIZE)).await {
                Ok(result) => result?,
                Err(_) => {
                    summary.stopped_early = true;
                    break;
                }
            };
        let Some(last) = candidates.last() else { break };
        after.clone_from(&last.account_id);

        for candidate in candidates {
            if cancel.is_cancelled() {
                return Err("account recovery lease lost".into());
            }
            let result = match timeout_at(deadline, check_candidate(state, &candidate)).await {
                Ok(result) => result,
                Err(_) => {
                    summary.stopped_early = true;
                    break 'scan;
                }
            };
            if cancel.is_cancelled() {
                return Err("account recovery lease lost".into());
            }
            summary.checked += 1;
            consecutive_failed_checks = match &result {
                Ok(_) => 0,
                Err(_) => consecutive_failed_checks + 1,
            };
            let record = match result {
                Ok(RecoveryOutcome::Unchanged) => {
                    summary.unchanged += 1;
                    PendingRecord::Clear
                }
                Ok(RecoveryOutcome::Verified(restoration)) => match writes {
                    ScanWrites::DryRun => {
                        summary.restorable += 1;
                        tracing::info!(
                            account_id = %candidate.account_id,
                            proposals_changed = restoration.changed_proposals().count(),
                            "Dry run: the account would be restored"
                        );
                        restoration.log_proposal_changes(&candidate.account_id, true);
                        PendingRecord::Clear
                    }
                    ScanWrites::Apply(lease) => {
                        match store
                            .apply(
                                &candidate,
                                &restoration.state.state_json,
                                &restoration.auth,
                                &restoration.proposals,
                                state.clock.now(),
                                lease,
                            )
                            .await
                        {
                            Ok(true) => {
                                summary.restored += 1;
                                tracing::info!(
                                    account_id = %candidate.account_id,
                                    proposals_changed = restoration.changed_proposals().count(),
                                    "Restored chain-verified account state and signers"
                                );
                                restoration.log_proposal_changes(&candidate.account_id, false);
                                PendingRecord::Clear
                            }
                            Ok(false) => {
                                summary.retryable += 1;
                                tracing::info!(account_id = %candidate.account_id, "Account became ineligible or changed during recovery");
                                PendingRecord::Write(
                                    PendingOutcome::Deferred,
                                    "the account changed or became ineligible during recovery"
                                        .into(),
                                )
                            }
                            Err(reason) => {
                                summary.failed += 1;
                                tracing::warn!(account_id = %candidate.account_id, %reason, "Account recovery transaction failed");
                                PendingRecord::Write(
                                    PendingOutcome::Deferred,
                                    "restore transaction failed; see Guardian logs".into(),
                                )
                            }
                        }
                    }
                },
                Ok(RecoveryOutcome::Retryable(reason)) => {
                    summary.retryable += 1;
                    tracing::info!(account_id = %candidate.account_id, %reason, dry_run, "Account recovery deferred until a later startup");
                    PendingRecord::Write(PendingOutcome::Deferred, reason)
                }
                Ok(RecoveryOutcome::Unresolved(reason)) => {
                    summary.unresolved += 1;
                    tracing::warn!(account_id = %candidate.account_id, %reason, dry_run, "Account recovery needs additional evidence");
                    PendingRecord::Write(PendingOutcome::Unresolved, reason)
                }
                Err(reason) => {
                    summary.failed += 1;
                    tracing::warn!(account_id = %candidate.account_id, %reason, dry_run, "Account recovery could not check the candidate");
                    PendingRecord::Write(
                        PendingOutcome::Deferred,
                        "account verification failed; see Guardian logs".into(),
                    )
                }
            };
            if let ScanWrites::Apply(lease) = writes {
                let recorded = match record {
                    PendingRecord::Clear => {
                        bounded_db_call(
                            "clear a pending recovery record",
                            store.clear_pending(&candidate.account_id, lease),
                        )
                        .await
                    }
                    PendingRecord::Write(outcome, reason) => {
                        bounded_db_call(
                            "write a pending recovery record",
                            store.record_pending(&candidate, outcome, &reason, lease),
                        )
                        .await
                    }
                };
                if let Err(reason) = recorded {
                    summary.failed += 1;
                    tracing::warn!(account_id = %candidate.account_id, %reason, "Account recovery could not update the pending record");
                }
            }
            summary.last_checked = Some(candidate.account_id);
            if consecutive_failed_checks == 0 {
                position_before_failed_checks.clone_from(&summary.last_checked);
            }
            if consecutive_failed_checks >= MAX_CONSECUTIVE_FAILED_CHECKS {
                tracing::warn!(
                    consecutive_failed_checks,
                    "Account recovery stopped early after repeated failed checks"
                );
                summary.last_checked = position_before_failed_checks;
                summary.stopped_early = true;
                break 'scan;
            }
            if Instant::now() >= deadline {
                summary.stopped_early = true;
                break 'scan;
            }
        }
    }
    let remaining = if summary.stopped_early {
        let position = summary.last_checked.as_deref().unwrap_or(resume_after);
        timeout(
            REMAINING_COUNT_BUDGET,
            store.remaining_candidates(network_type, position),
        )
        .await
        .ok()
        .and_then(Result::ok)
    } else {
        Some(0)
    };
    tracing::info!(
        dry_run,
        checked = summary.checked,
        restored = summary.restored,
        restorable = summary.restorable,
        unchanged = summary.unchanged,
        retryable = summary.retryable,
        unresolved = summary.unresolved,
        failed = summary.failed,
        stopped_early = summary.stopped_early,
        ?remaining,
        "Finished account recovery scan"
    );
    Ok(summary)
}

async fn check_candidate(
    state: &AppState,
    candidate: &RecoveryCandidate,
) -> Result<RecoveryOutcome, String> {
    let chain = state
        .network_client
        .verify_commitment(
            &candidate.account_id,
            &candidate.stored_commitment,
            RpcReadMode::Configured,
        )
        .await?;
    if matches!(chain, StateVerification::Match) {
        return Ok(RecoveryOutcome::Unchanged);
    }
    if matches!(chain, StateVerification::Absent) {
        if candidate.canonical_nonce.is_some() {
            return Ok(RecoveryOutcome::Retryable(
                "the configured node has no record of the account".into(),
            ));
        }
        if holds_first_state(state, candidate).await? {
            return Ok(RecoveryOutcome::Unchanged);
        }
    }
    if let Some(unsettled_work) = &candidate.unsettled_work {
        return Ok(RecoveryOutcome::Retryable(unsettled_work.clone()));
    }

    let (Some(canonical_nonce), Some(target)) = (
        candidate.canonical_nonce,
        candidate.canonical_commitment.as_deref(),
    ) else {
        let reason = match chain {
            StateVerification::Absent => {
                "the configured node has no record of the account and no canonical history is held"
            }
            StateVerification::Match | StateVerification::Mismatch { .. } => {
                "no canonical history supplies a recovery target"
            }
        };
        return Ok(RecoveryOutcome::Unresolved(reason.into()));
    };
    if !matches!(&chain, StateVerification::Mismatch { on_chain } if on_chain == target) {
        return Ok(RecoveryOutcome::Retryable(
            "latest canonical delta does not match the current chain head".into(),
        ));
    }
    let Some(count) = u32::try_from(canonical_nonce)
        .ok()
        .filter(|count| (1..=MAX_DELTAS).contains(count))
    else {
        return Ok(RecoveryOutcome::Unresolved(
            "canonical history exceeds the recovery limit".into(),
        ));
    };
    let history = state
        .storage
        .list_canonical_deltas_paged(&candidate.account_id, count, None)
        .await;
    let deltas = match history {
        Ok(deltas) => deltas,
        Err(reason) if is_record_corruption(&reason) => {
            return Ok(RecoveryOutcome::Unresolved(reason));
        }
        Err(reason) => return Err(reason),
    };
    if deltas.len() != count as usize {
        return Ok(RecoveryOutcome::Unresolved(
            "missing canonical deltas from account creation".into(),
        ));
    }

    let network = state.network_client.clone();
    let account_id = candidate.account_id.clone();
    let target = target.to_owned();
    let proof = reconstructor()
        .run_background(move || prove_recovery(network.as_ref(), &account_id, &target, deltas))
        .await;
    let (applied, auth) = match proof {
        Ok(proof) => proof,
        Err(ReconstructError::Operation(reason)) => return Ok(RecoveryOutcome::Unresolved(reason)),
        Err(ReconstructError::Task(reason)) => {
            return Err(format!("state reconstruction failed: {reason}"));
        }
    };

    let scheme = auth.guardian_scheme();
    if applied.guardian_commitment.as_deref() != Some(state.ack.commitment(&scheme).as_str()) {
        return Ok(RecoveryOutcome::Unresolved(
            "recovered state is bound to a different Guardian key".into(),
        ));
    }

    match state
        .network_client
        .verify_commitment(
            &candidate.account_id,
            &applied.commitment,
            RpcReadMode::Configured,
        )
        .await?
    {
        StateVerification::Match => {}
        StateVerification::Mismatch { .. } | StateVerification::Absent => {
            return Ok(RecoveryOutcome::Retryable(
                "reconstructed state does not match the current chain head".into(),
            ));
        }
    }

    let proposals = match state
        .storage
        .pull_pending_proposals(&candidate.account_id)
        .await
    {
        Ok(pending) => ProposalReview::Reviewed(proposals::review(&applied, &pending)),
        Err(reason) if is_record_corruption(&reason) => ProposalReview::Unreadable,
        Err(reason) => return Err(reason),
    };
    Ok(RecoveryOutcome::Verified(Box::new(Restoration {
        state: applied,
        auth,
        proposals,
    })))
}

/// Whether the stored state is a first state, which stands without a record on the network.
async fn holds_first_state(
    state: &AppState,
    candidate: &RecoveryCandidate,
) -> Result<bool, String> {
    if candidate.stored_nonce.is_some_and(|nonce| nonce != 0) {
        return Ok(false);
    }
    match state.storage.pull_state(&candidate.account_id).await {
        Ok(stored) => Ok(state
            .network_client
            .validate_first_state(&stored.state_json)
            .is_ok()),
        Err(reason) if is_record_corruption(&reason) => Ok(false),
        Err(reason) => Err(reason),
    }
}

fn prove_recovery(
    network: &dyn NetworkClient,
    account_id: &str,
    target: &str,
    deltas: Vec<DeltaObject>,
) -> Result<(AppliedState, Auth), String> {
    let applied = reconstruct(network, account_id, target, deltas)?;
    let binding = network
        .account_auth_binding(&applied.state_json)?
        .ok_or("missing account auth binding")?;
    network.validate_multisig_auth(&applied.state_json)?;
    let scheme = binding
        .guardian_scheme
        .ok_or("missing on-chain Guardian signature scheme")?;
    let auth = recovered_auth(scheme, &applied)?;
    Ok((applied, auth))
}

fn reconstruct(
    network: &dyn NetworkClient,
    account_id: &str,
    target: &str,
    deltas: Vec<DeltaObject>,
) -> Result<AppliedState, String> {
    let mut reconstructed: Option<AppliedState> = None;
    for (index, delta) in deltas.into_iter().rev().enumerate() {
        if delta.account_id != account_id
            || delta.nonce != index as u64 + 1
            || !delta.status.is_canonical()
        {
            return Err("canonical history is not contiguous from account creation".into());
        }
        let applied = match reconstructed {
            None => network.apply_delta(&serde_json::Value::Null, &delta.delta_payload)?,
            Some(previous) => network.verify_and_apply_delta(
                &delta.prev_commitment,
                &previous.state_json,
                &delta.delta_payload,
            )?,
        };
        if delta.new_commitment.as_deref() != Some(applied.commitment.as_str())
            || applied.nonce != Some(delta.nonce)
        {
            return Err("reconstructed delta does not match its canonical row".into());
        }
        reconstructed = Some(applied);
    }
    let applied = reconstructed.ok_or("canonical history is empty")?;
    let head = network.get_state_head(account_id, &applied.state_json)?;
    if head.commitment != target || head.nonce != applied.nonce {
        return Err("reconstructed state does not match the expected account head".into());
    }
    Ok(applied)
}

fn recovered_auth(scheme: SignatureScheme, state: &AppliedState) -> Result<Auth, String> {
    if state.cosigners.is_empty() || state.cosigners.iter().any(|signer| signer.scheme.is_none()) {
        return Err("recovered signer set is empty or has an unknown signature scheme".into());
    }
    let baseline = match scheme {
        SignatureScheme::Falcon => Auth::MidenFalconRpo {
            cosigner_commitments: Vec::new(),
        },
        SignatureScheme::Ecdsa => Auth::MidenEcdsa {
            cosigner_commitments: Vec::new(),
        },
    };
    Ok(baseline.with_chain_cosigners(state.cosigners.clone()))
}

mod proposals;

#[cfg(test)]
mod postgres_tests;
