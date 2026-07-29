use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::canonicalization::CanonicalizationConfig;
use crate::coordination::{AlwaysLeader, CANONICALIZATION_LEASE, LeaderElector, Lease};
use crate::delta_object::{DeltaObject, DeltaStatus, RetainReason};
use crate::error::{GuardianError, Result};
use crate::metadata::AccountMetadata;
use crate::network::StateVerification;
use crate::state::AppState;
use crate::state_object::StateObject;
use crate::storage::{
    CandidatePromotion, CanonicalWrite, DeltaStatusKind, LeaseFence, PromoteWrite,
    RecentCandidateCursor,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[path = "reconciliation.rs"]
mod reconciliation;

const FAST_PROMOTION_PAGE_SIZE: u32 = 100;

#[derive(Default)]
pub(super) struct FastPromotionState {
    cursor: Mutex<Option<RecentCandidateCursor>>,
}

/// Cross-pass state for the reconcile pass: the rotation cursor that
/// keeps the per-pass account page fair when the recoverable backlog
/// exceeds one page.
#[derive(Default)]
pub(super) struct ReconcileState {
    cursor: Mutex<Option<String>>,
}

/// Shared cross-pass state plus the per-pass deadline, handed to the
/// processor by the worker. The deadline applies to the bounded passes
/// (fast promotion and reconcile); the full pass runs to completion.
pub(super) struct PassControls {
    pub fast_state: Arc<FastPromotionState>,
    pub reconcile_state: Arc<ReconcileState>,
    pub deadline: Option<Instant>,
    /// Age-derived per-account backoff for the reconcile pass. The
    /// worker enables it; the test processor disables it so
    /// process-now endpoints reconcile deterministically.
    pub reconcile_backoff: bool,
}

impl PassControls {
    fn unbounded() -> Self {
        Self {
            fast_state: Arc::new(FastPromotionState::default()),
            reconcile_state: Arc::new(ReconcileState::default()),
            deadline: None,
            reconcile_backoff: false,
        }
    }
}

impl FastPromotionState {
    fn cursor(&self) -> Option<RecentCandidateCursor> {
        self.cursor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn set_cursor(&self, cursor: Option<RecentCandidateCursor>) {
        *self
            .cursor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = cursor;
    }
}

impl ReconcileState {
    fn cursor(&self) -> Option<String> {
        self.cursor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn set_cursor(&self, cursor: Option<String>) {
        *self
            .cursor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = cursor;
    }
}

/// A leader handle for a single canonicalization pass: who we are, the fence we
/// hold, and a cancellation signal tripped when the lease is lost mid-pass.
struct PassLease {
    leader: Arc<dyn LeaderElector>,
    lease: Lease,
    cancel: CancellationToken,
}

impl PassLease {
    /// Single-process default (filesystem / tests): always the leader, never
    /// cancelled.
    fn single_process() -> Self {
        Self {
            leader: Arc::new(AlwaysLeader::new(CANONICALIZATION_LEASE, "single-process")),
            lease: Lease {
                name: CANONICALIZATION_LEASE.to_string(),
                holder_id: "single-process".to_string(),
                fence_token: 0,
                expires_at: DateTime::<Utc>::MAX_UTC,
            },
            cancel: CancellationToken::new(),
        }
    }
}

/// What one canonicalization pass actually did. Per-account failures and
/// lease-loss cancellation are absorbed by the pass loop, so `Ok(())`
/// alone could not distinguish a clean pass from a degraded one — the
/// worker's run-outcome metric needs the distinction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassSummary {
    pub accounts: usize,
    pub failed_accounts: usize,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcessingMode {
    Full,
    PromoteRecent {
        max_age_seconds: u64,
    },
    /// Dedicated background sweep over recoverable deltas (issue #345):
    /// retained rows and recent client-abandoned discards. Runs on its
    /// own, slower cadence so per-account chain probes never crowd out
    /// ordinary candidate processing in the full pass.
    ReconcileRecoverable,
}

#[async_trait]
pub trait Processor: Send + Sync {
    async fn process_all_accounts(&self) -> Result<PassSummary>;

    #[allow(dead_code)]
    async fn process_account(&self, account_id: &str) -> Result<()>;
}

/// Record one candidate-processing outcome.
fn record_candidate_outcome(outcome: crate::metrics::labels::CandidateOutcome) {
    metrics::counter!(
        crate::metrics::names::CANONICALIZATION_CANDIDATES_TOTAL,
        crate::metrics::names::LABEL_OUTCOME => outcome.as_str()
    )
    .increment(1);
}

struct DeltasProcessorBase {
    state: AppState,
    pass: PassLease,
    mode: ProcessingMode,
    max_retries: u32,
    submission_grace_period_seconds: u64,
    divergence_confirmations: u32,
    abandon_quarantine_seconds: u64,
    abandon_quarantine_checks: u32,
    max_concurrent_accounts: usize,
    retained_ttl_seconds: u64,
    reconcile_interval_seconds: u64,
    reconcile_page_size: u32,
    reconcile_backoff: bool,
    fast_promotion_state: Arc<FastPromotionState>,
    reconcile_state: Arc<ReconcileState>,
    pass_deadline: Option<Instant>,
}

impl DeltasProcessorBase {
    /// Fence descriptor attached to every custody-state write: the canonical
    /// promotion, the discard, and the retry / divergence-streak status
    /// updates. Fenced backends (Postgres) validate it against the lease row
    /// at the write boundary. A transition already in progress may finish
    /// during leadership transfer; account serialization and conditional
    /// candidate/state updates keep that overlap safe. A holder superseded
    /// before validation is refused, and a delayed write cannot demote or
    /// delete a delta another owner already promoted.
    /// `None` for single-process electors, whose leases have no shared-store
    /// row. Trailing cleanup (proposal deletion, release-on-switch) is
    /// intentionally unfenced but not blind: proposal deletion is idempotent
    /// and only follows a committed transition.
    fn fence(&self) -> Option<LeaseFence> {
        self.pass.leader.supports_fencing().then(|| LeaseFence {
            lease_name: self.pass.lease.name.clone(),
            holder_id: self.pass.lease.holder_id.clone(),
            fence_token: self.pass.lease.fence_token,
        })
    }

    /// Map a superseded-lease outcome to the error the pass surfaces; the
    /// renewal task cancels the pass at the next checkpoint.
    fn stale_lease_error(delta: &DeltaObject) -> GuardianError {
        tracing::warn!(
            account_id = %delta.account_id,
            nonce = delta.nonce,
            "Canonicalization lease lost; write refused by storage fence"
        );
        GuardianError::StorageError("canonicalization lease lost; write refused".to_string())
    }

    /// Log a stale-candidate outcome: another owner already promoted or
    /// discarded this delta, so the write was a no-op by design.
    fn log_not_candidate(delta: &DeltaObject, operation: &str) {
        tracing::warn!(
            account_id = %delta.account_id,
            nonce = delta.nonce,
            operation,
            "Delta is no longer a candidate; skipping superseded write"
        );
    }

    fn candidate_age_seconds(&self, delta: &DeltaObject, now: DateTime<Utc>) -> Option<u64> {
        let DeltaStatus::Candidate { timestamp, .. } = &delta.status else {
            return None;
        };

        let candidate_at = DateTime::parse_from_rfc3339(timestamp).ok()?;
        let age = now.signed_duration_since(candidate_at.with_timezone(&Utc));
        Some(age.num_seconds().max(0) as u64)
    }

    fn should_process_candidate(&self, delta: &DeltaObject) -> bool {
        match self.mode {
            ProcessingMode::Full => true,
            ProcessingMode::PromoteRecent { max_age_seconds } => self
                .candidate_age_seconds(delta, self.state.clock.now())
                .is_some_and(|age| age < max_age_seconds),
            // The reconcile pass never processes candidates — it skips
            // any account that has one in flight.
            ProcessingMode::ReconcileRecoverable => false,
        }
    }

    async fn process_all_accounts(&self) -> Result<PassSummary> {
        match self.mode {
            ProcessingMode::Full => self.process_full_pass().await,
            ProcessingMode::PromoteRecent { max_age_seconds } => {
                self.process_recent_pass(max_age_seconds).await
            }
            ProcessingMode::ReconcileRecoverable => self.process_reconcile_pass().await,
        }
    }

    async fn process_full_pass(&self) -> Result<PassSummary> {
        let account_ids = self
            .state
            .metadata
            .list_with_pending_candidates()
            .await
            .map_err(|e| GuardianError::StorageError(format!("Failed to list accounts: {e}")))?;

        tracing::info!(
            accounts_with_candidates = account_ids.len(),
            "Running canonicalization process"
        );
        metrics::gauge!(crate::metrics::names::CANONICALIZATION_PASS_ACCOUNTS)
            .set(account_ids.len() as f64);

        // Account concurrency overlaps independent RPC waits; synchronous
        // reconstruction is separately bounded by `Reconstructor`. Candidates
        // within one account stay strictly sequential (nonce order), and every
        // custody write remains individually fenced.
        let accounts = account_ids.len();
        let failed_accounts = futures::stream::iter(account_ids)
            .map(|account_id| async move { self.process_account_absorbing(&account_id).await })
            .buffer_unordered(self.max_concurrent_accounts.max(1))
            .fold(0, |failed, account_failed| async move {
                failed + usize::from(account_failed)
            })
            .await;

        Ok(self.pass_summary(accounts, failed_accounts))
    }

    async fn process_recent_pass(&self, max_age_seconds: u64) -> Result<PassSummary> {
        let started = Instant::now();
        let now = self.state.clock.now();
        let cutoff = i64::try_from(max_age_seconds)
            .ok()
            .and_then(chrono::TimeDelta::try_seconds)
            .and_then(|window| now.checked_sub_signed(window))
            .unwrap_or(DateTime::<Utc>::MIN_UTC);
        let mut cursor = self.fast_promotion_state.cursor();
        let resumed_from_cursor = cursor.is_some();
        let mut pages = 0;
        let mut candidates_seen = 0;
        let mut accounts = 0;
        let mut failed_accounts = 0;

        while !self.admission_closed() {
            let candidates = self
                .state
                .storage
                .pull_recent_candidate_deltas(cutoff, cursor.as_ref(), FAST_PROMOTION_PAGE_SIZE)
                .await
                .map_err(|e| {
                    GuardianError::StorageError(format!("Failed to pull recent candidates: {e}"))
                })?;
            if candidates.is_empty() {
                self.fast_promotion_state.set_cursor(None);
                break;
            }
            if self.admission_closed() {
                break;
            }

            let page_len = candidates.len();
            pages += 1;
            candidates_seen += page_len;
            let Some(last_candidate) = candidates.last() else {
                break;
            };
            let next_cursor = Self::recent_candidate_cursor(last_candidate);
            let (page_accounts, page_failed_accounts) = self.process_recent_page(candidates).await;
            accounts += page_accounts;
            failed_accounts += page_failed_accounts;

            let Some(next_cursor) = next_cursor else {
                self.fast_promotion_state.set_cursor(None);
                break;
            };
            cursor = Some(next_cursor);
            // Stored before the loop re-checks admission: a pass cut short
            // by its deadline resumes after this page on the next tick
            // instead of restarting at the window's oldest rows.
            self.fast_promotion_state.set_cursor(cursor.clone());

            if page_len < FAST_PROMOTION_PAGE_SIZE as usize {
                self.fast_promotion_state.set_cursor(None);
                break;
            }
        }

        tracing::debug!(
            pages,
            candidates = candidates_seen,
            account_batches = accounts,
            failed_accounts,
            resumed_from_cursor,
            cursor_retained = self.fast_promotion_state.cursor().is_some(),
            deadline_reached = self.pass_deadline_reached(),
            duration_seconds = started.elapsed().as_secs_f64(),
            "Fast-promotion pass completed"
        );

        Ok(self.pass_summary(accounts, failed_accounts))
    }

    /// Pages are keyed by status timestamp, so a retried candidate (whose
    /// timestamp was refreshed) can arrive on a later page than its
    /// account's earlier nonces. The per-account nonce sort below only
    /// orders within a page; an out-of-order candidate fails the
    /// reconstruction-equality check and defers to the full pass — fast
    /// promotion intentionally degrades for retried candidates instead of
    /// re-sorting across pages.
    async fn process_recent_page(&self, candidates: Vec<DeltaObject>) -> (usize, usize) {
        let mut candidates_by_account = BTreeMap::<String, Vec<DeltaObject>>::new();
        for candidate in candidates
            .into_iter()
            .filter(|candidate| self.should_process_candidate(candidate))
        {
            candidates_by_account
                .entry(candidate.account_id.clone())
                .or_default()
                .push(candidate);
        }
        for candidates in candidates_by_account.values_mut() {
            candidates.sort_by_key(|candidate| candidate.nonce);
        }

        let accounts = candidates_by_account.len();
        tracing::debug!(
            accounts_with_recent_candidates = accounts,
            "Running recent-candidate promotion pass"
        );
        let failed_accounts = futures::stream::iter(candidates_by_account)
            .map(|(account_id, candidates)| async move {
                self.process_candidates_absorbing(&account_id, candidates)
                    .await
            })
            .buffer_unordered(self.max_concurrent_accounts.max(1))
            .fold(0, |failed, account_failed| async move {
                failed + usize::from(account_failed)
            })
            .await;

        (accounts, failed_accounts)
    }

    /// Build the keyset cursor for the next page from the current page's
    /// last candidate. Both shipped backends guarantee the timestamp
    /// parses (Postgres refuses to write a malformed one; the filesystem
    /// backend skips unparseable rows), so a failure here means a new or
    /// drifted backend — end pagination for this pass rather than erroring
    /// a pass that reruns every few seconds; the full pass owns the row.
    fn recent_candidate_cursor(delta: &DeltaObject) -> Option<RecentCandidateCursor> {
        let timestamp = match DateTime::parse_from_rfc3339(delta.status.timestamp()) {
            Ok(timestamp) => timestamp.with_timezone(&Utc),
            Err(error) => {
                tracing::warn!(
                    account_id = %delta.account_id,
                    nonce = delta.nonce,
                    error = %error,
                    "Recent candidate has an invalid status timestamp; ending fast-pass pagination"
                );
                return None;
            }
        };
        Some(RecentCandidateCursor {
            last_status_timestamp: timestamp,
            last_account_id: delta.account_id.clone(),
            last_nonce: delta.nonce,
        })
    }

    fn admission_closed(&self) -> bool {
        self.pass.cancel.is_cancelled() || self.pass_deadline_reached()
    }

    fn pass_deadline_reached(&self) -> bool {
        self.pass_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    fn pass_summary(&self, accounts: usize, failed_accounts: usize) -> PassSummary {
        if self.pass.cancel.is_cancelled() {
            tracing::warn!(
                "Canonicalization pass cancelled (lease lost); remaining accounts skipped"
            );
        }

        PassSummary {
            accounts,
            failed_accounts,
            cancelled: self.pass.cancel.is_cancelled(),
        }
    }

    /// One account inside a concurrent pass: cancellation-checked at
    /// admission (in-flight tasks stop at their own checkpoints), errors
    /// absorbed into a failed flag so one account never sinks the pass.
    async fn process_account_absorbing(&self, account_id: &str) -> bool {
        if self.pass.cancel.is_cancelled() {
            return false;
        }
        match self.process_account(account_id).await {
            Ok(()) => false,
            Err(e) => {
                tracing::error!(
                    account_id = %account_id,
                    error = %e,
                    "Failed to process canonicalizations for account"
                );
                true
            }
        }
    }

    async fn process_candidates_absorbing(
        &self,
        account_id: &str,
        candidates: Vec<DeltaObject>,
    ) -> bool {
        if self.admission_closed() {
            return false;
        }
        match self.process_candidates(account_id, candidates).await {
            Ok(()) => false,
            Err(e) => {
                tracing::error!(
                    account_id = %account_id,
                    error = %e,
                    "Failed to process recent canonicalizations for account"
                );
                true
            }
        }
    }

    async fn process_account(&self, account_id: &str) -> Result<()> {
        let account_metadata = self.fetch_account_metadata(account_id).await?;

        let candidates = self
            .state
            .storage
            .pull_candidate_deltas(account_id)
            .await
            .map_err(|e| GuardianError::StorageError(format!("Failed to pull deltas: {e}")))?;

        // Heal a stale pending-candidate flag: the flag can be left set
        // when a best-effort flag-clear fails after the candidate delta
        // was already removed. Without this, the account stays in
        // `list_with_pending_candidates` and is rescanned on every tick
        // forever. The conditional clear re-checks the delta store, so a
        // candidate committed concurrently is never masked.
        if candidates.is_empty() && account_metadata.has_pending_candidate {
            tracing::warn!(
                account_id = %account_id,
                "Account flagged with pending candidate but no candidate delta \
                 exists; clearing stale flag"
            );
            let now = self.state.clock.now_rfc3339();
            if let Err(e) = self
                .state
                .metadata
                .clear_pending_candidate_if_none(account_id, &now)
                .await
            {
                tracing::warn!(
                    account_id = %account_id,
                    error = %e,
                    "Failed to clear stale has_pending_candidate flag"
                );
            }
            return Ok(());
        }

        // Recoverable deltas (issue #345) are deliberately NOT handled
        // here: the dedicated reconcile pass owns them, on its own
        // slower cadence with per-account backoff, so their per-account
        // chain probes never delay ordinary candidate processing.
        if candidates.is_empty() {
            return Ok(());
        }

        metrics::counter!(crate::metrics::names::CANONICALIZATION_DELTAS_FETCHED_TOTAL)
            .increment(candidates.len() as u64);

        self.process_candidate_list(account_id, candidates).await
    }

    async fn process_candidates(
        &self,
        account_id: &str,
        candidates: Vec<DeltaObject>,
    ) -> Result<()> {
        self.fetch_account_metadata(account_id).await?;
        self.process_candidate_list(account_id, candidates).await
    }

    async fn fetch_account_metadata(&self, account_id: &str) -> Result<AccountMetadata> {
        self.state
            .metadata
            .get(account_id)
            .await
            .map_err(|e| GuardianError::StorageError(format!("Failed to get metadata: {e}")))?
            .ok_or_else(|| GuardianError::InvalidInput("Account metadata not found".to_string()))
    }

    async fn process_candidate_list(
        &self,
        account_id: &str,
        candidates: Vec<DeltaObject>,
    ) -> Result<()> {
        let candidates = candidates
            .into_iter()
            .filter(|delta| self.should_process_candidate(delta))
            .collect::<Vec<_>>();

        match self.mode {
            ProcessingMode::Full => tracing::info!(
                account_id = %account_id,
                candidates = candidates.len(),
                "Processing delta candidates"
            ),
            ProcessingMode::PromoteRecent { .. } | ProcessingMode::ReconcileRecoverable => {
                tracing::debug!(
                    account_id = %account_id,
                    candidates = candidates.len(),
                    "Processing recent delta candidates"
                )
            }
        }
        let mut first_error = None;
        for (index, delta) in candidates.into_iter().enumerate() {
            if self.admission_closed() {
                if self.pass.cancel.is_cancelled() {
                    tracing::warn!(
                        account_id = %account_id,
                        "Canonicalization pass cancelled (lease lost); stopping before next candidate"
                    );
                } else {
                    tracing::debug!(
                        account_id = %account_id,
                        "Fast-promotion deadline reached; stopping before next candidate"
                    );
                }
                break;
            }
            // A candidate can only verify from the stored base. When a
            // predecessor fails to canonicalize (deferred, retried,
            // retained), every later nonce still chains from a state the
            // store has not reached — reconstructing it would fail or
            // emit false commitment-mismatch signals and burn budget for
            // nothing, so the account's pass stops at the break in the
            // chain. The first candidate needs no check (admission
            // guaranteed its base, and the fenced promotion still guards
            // a concurrent move); a read failure does not stop the pass —
            // the per-candidate path re-reads the state and surfaces the
            // error with proper accounting.
            if index > 0
                && let Ok(current_state) = self.state.storage.pull_state(account_id).await
                && delta.prev_commitment != current_state.commitment
            {
                tracing::info!(
                    account_id = %account_id,
                    nonce = delta.nonce,
                    "Candidate does not chain from the stored state; \
                     stopping this account's pass at the first unresolved nonce"
                );
                break;
            }
            let nonce = delta.nonce;
            if let Err(e) = self.process_candidate(delta).await {
                tracing::error!(
                    account_id = %account_id,
                    nonce = nonce,
                    error = %e,
                    "Failed to canonicalize delta"
                );
                first_error.get_or_insert(e);
            }
        }

        if let Some(error) = first_error {
            return Err(error);
        }

        Ok(())
    }

    async fn process_candidate(&self, delta: DeltaObject) -> Result<()> {
        match self.mode {
            // The reconcile pass filters out every candidate before this
            // point; the full path is the safe behavior if one ever slips
            // through.
            ProcessingMode::Full | ProcessingMode::ReconcileRecoverable => {
                self.process_full_candidate(delta).await
            }
            ProcessingMode::PromoteRecent { .. } => self.process_recent_candidate(delta).await,
        }
    }

    async fn process_full_candidate(&self, delta: DeltaObject) -> Result<()> {
        if let Some(age) = self.candidate_age_seconds(&delta, self.state.clock.now()) {
            metrics::histogram!(crate::metrics::names::CANONICALIZATION_CANDIDATE_AGE_SECONDS)
                .record(age as f64);
        }

        let storage_backend = self.state.storage.clone();

        let current_state = storage_backend
            .pull_state(&delta.account_id)
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to get current state: {e}"))
            })?;

        let (new_state_json, recomputed_commitment) = {
            let client = self.state.network_client.clone();
            let prev_state_json = current_state.state_json;
            let delta_payload = Arc::new(delta.delta_payload.clone());
            crate::network::reconstructor()
                .run_background(move || client.apply_delta(&prev_state_json, &delta_payload))
                .await?
        };

        let verify_result = self
            .state
            .network_client
            .verify_commitment(&delta.account_id, &recomputed_commitment)
            .await;

        match verify_result {
            // Verification proved the recomputed commitment is what the
            // chain holds, so it — not the client-claimed `new_commitment` —
            // is what promotion persists. A differing (or absent) claim is a
            // client defect worth surfacing, never a reason to strand a
            // landed transaction as a candidate forever.
            Ok(StateVerification::Match) => {
                if delta.new_commitment.as_deref() != Some(recomputed_commitment.as_str()) {
                    tracing::warn!(
                        account_id = %delta.account_id,
                        nonce = delta.nonce,
                        claimed = ?delta.new_commitment,
                        recomputed = %recomputed_commitment,
                        "Client-claimed commitment is missing or differs from the verified \
                         recomputed commitment; promoting with the verified one"
                    );
                    metrics::counter!(
                        crate::metrics::names::CANONICALIZATION_COMMITMENT_MISMATCHES_TOTAL
                    )
                    .increment(1);
                }
                self.canonicalize_verified_delta(
                    delta,
                    new_state_json,
                    recomputed_commitment,
                    crate::metrics::labels::CandidateOutcome::Canonicalized,
                )
                .await
            }
            // The chain has never seen this account: its first transaction
            // has not landed yet — the not-yet-landed case, categorically
            // distinct from divergence (where the account moved to a
            // *different* state past the candidate's base). Treat it as
            // defer/retry and clear any divergence streak a lagging node
            // may have started.
            Ok(StateVerification::Absent) => {
                let delta = self.reset_divergence_streak(delta).await?;
                // An absent account is equally strong "did not land"
                // evidence as an at-base read: a dead FIRST transaction
                // must be abandonable too, not held for the grace window.
                if delta.status.abandon_requested_at().is_some() {
                    return self.handle_abandon_confirmation(delta).await;
                }
                self.handle_unverified_candidate(delta, "account not yet on chain")
                    .await
            }
            // The account advanced past the state this candidate was built
            // on: its transaction is anchored to `prev_commitment`, so it can
            // never land anymore and the expected commitment is permanently
            // unsatisfiable. Waiting out the grace period would only keep the
            // account locked (every new proposal 409s meanwhile) — discard as
            // soon as the divergence is confirmed.
            Ok(StateVerification::Mismatch { on_chain }) if on_chain != delta.prev_commitment => {
                self.handle_diverged_candidate(delta, &on_chain).await
            }
            // On-chain still shows the candidate's base state: the
            // transaction simply has not landed yet. This read also proves
            // any earlier diverged observation was stale, so the
            // divergence streak starts over. A client abandon request is
            // resolved on this arm — an at-base observation is exactly the
            // evidence the abandon quarantine counts — and takes
            // precedence over the grace/retry deferral, which would
            // otherwise hold the abandon for the full grace window.
            Ok(StateVerification::Mismatch { on_chain }) => {
                let delta = self.reset_divergence_streak(delta).await?;
                if delta.status.abandon_requested_at().is_some() {
                    return self.handle_abandon_confirmation(delta).await;
                }
                self.handle_unverified_candidate(
                    delta,
                    &format!("on-chain commitment still at candidate base {on_chain}"),
                )
                .await
            }
            // The comparison itself failed (RPC error, malformed state):
            // no observation was made, so the divergence streak is left
            // untouched and the grace/retry behavior applies.
            Err(e) => self.handle_unverified_candidate(delta, &e).await,
        }
    }

    async fn process_recent_candidate(&self, delta: DeltaObject) -> Result<()> {
        let Some(claimed_commitment) = delta.new_commitment.clone() else {
            tracing::debug!(
                account_id = %delta.account_id,
                nonce = delta.nonce,
                "Recent candidate has no claimed commitment; leaving it to the full pass"
            );
            return Ok(());
        };

        match self
            .state
            .network_client
            .verify_commitment(&delta.account_id, &claimed_commitment)
            .await
        {
            Ok(StateVerification::Match) => {}
            Ok(_) => {
                tracing::debug!(
                    account_id = %delta.account_id,
                    nonce = delta.nonce,
                    "Recent candidate is not canonical yet; leaving lifecycle decisions to the full pass"
                );
                return Ok(());
            }
            Err(error) => return Err(GuardianError::NetworkError(error)),
        }

        let current_state = self
            .state
            .storage
            .pull_state(&delta.account_id)
            .await
            .map_err(|error| {
                GuardianError::StorageError(format!("Failed to get current state: {error}"))
            })?;
        let (new_state_json, recomputed_commitment) = {
            let client = self.state.network_client.clone();
            let prev_state_json = current_state.state_json;
            let delta_payload = Arc::new(delta.delta_payload.clone());
            crate::network::reconstructor()
                .run_background(move || client.apply_delta(&prev_state_json, &delta_payload))
                .await?
        };

        if recomputed_commitment != claimed_commitment {
            tracing::warn!(
                account_id = %delta.account_id,
                nonce = delta.nonce,
                claimed = %claimed_commitment,
                recomputed = %recomputed_commitment,
                "Client-claimed commitment differs from the reconstructed commitment; leaving it to the full pass"
            );
            metrics::counter!(crate::metrics::names::CANONICALIZATION_COMMITMENT_MISMATCHES_TOTAL)
                .increment(1);
            return Ok(());
        }

        self.canonicalize_verified_delta(
            delta,
            new_state_json,
            recomputed_commitment,
            crate::metrics::labels::CandidateOutcome::Canonicalized,
        )
        .await
    }

    /// Count one at-base observation toward resolving a client abandon
    /// request (issue #319), and finalize once the quarantine is
    /// satisfied: enough consecutive at-base observations AND enough wall
    /// time since the request for a late-landing transaction to surface.
    async fn handle_abandon_confirmation(&self, delta: DeltaObject) -> Result<()> {
        let requested_at = delta
            .status
            .abandon_requested_at()
            .unwrap_or_default()
            .to_string();
        let confirmations = delta.status.abandon_confirm_count() + 1;

        let now = self.state.clock.now();
        let request_age_seconds = DateTime::parse_from_rfc3339(&requested_at)
            .map(|at| {
                now.signed_duration_since(at.with_timezone(&Utc))
                    .num_seconds()
                    .max(0) as u64
            })
            .unwrap_or(u64::MAX);

        if confirmations >= self.abandon_quarantine_checks
            && request_age_seconds >= self.abandon_quarantine_seconds
        {
            return self.finalize_abandoned_candidate(delta).await;
        }

        tracing::info!(
            account_id = %delta.account_id,
            nonce = delta.nonce,
            confirmations,
            abandon_quarantine_checks = self.abandon_quarantine_checks,
            request_age_seconds,
            abandon_quarantine_seconds = self.abandon_quarantine_seconds,
            "Abandon requested and on-chain still at candidate base; \
             deferring until the quarantine is satisfied"
        );

        let new_status = delta.status.with_incremented_abandon_confirm();
        let outcome = self
            .state
            .storage
            .update_candidate_status(
                &delta.account_id,
                delta.nonce,
                new_status,
                self.fence().as_ref(),
            )
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to update delta status: {e}"))
            })?;
        match outcome {
            CanonicalWrite::Applied => {}
            CanonicalWrite::StaleLease => return Err(Self::stale_lease_error(&delta)),
            CanonicalWrite::NotCandidate => Self::log_not_candidate(&delta, "abandon_confirm"),
        }

        Ok(())
    }

    /// Resolve a confirmed abandon: finalize the matching proposal first,
    /// transition the delta to `Discarded { reason: ClientAbandoned }`
    /// (kept as history), then release the pending-candidate flag. Any
    /// cleanup failure leaves the candidate in place for the next worker
    /// run to retry.
    async fn finalize_abandoned_candidate(&self, delta: DeltaObject) -> Result<()> {
        // Shared cleanup helper: a failed delete (or an unverifiable
        // proposal read) defers the finalize to the next worker run. An
        // UNDERIVABLE proposal id proceeds instead — the condition is
        // permanent, so deferring on it would wedge the abandon forever
        // without ever fixing the proposal.
        if !self.delete_matching_proposal(&delta).await {
            return Ok(());
        }

        let now = self.state.clock.now_rfc3339();
        let outcome = self
            .state
            .storage
            .update_candidate_status(
                &delta.account_id,
                delta.nonce,
                DeltaStatus::discarded_client_abandoned(now.clone()),
                self.fence().as_ref(),
            )
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to discard abandoned delta: {e}"))
            })?;
        match outcome {
            CanonicalWrite::Applied => {}
            CanonicalWrite::StaleLease => return Err(Self::stale_lease_error(&delta)),
            CanonicalWrite::NotCandidate => {
                Self::log_not_candidate(&delta, "abandon_finalize");
                return Ok(());
            }
        }

        if let Err(e) = self
            .state
            .metadata
            .clear_pending_candidate_if_none(&delta.account_id, &now)
            .await
        {
            tracing::warn!(
                account_id = %delta.account_id,
                error = %e,
                "Failed to clear has_pending_candidate flag after abandon; \
                 the stale-flag heal clears it on a later run"
            );
        }

        record_candidate_outcome(crate::metrics::labels::CandidateOutcome::Abandoned);
        tracing::info!(
            account_id = %delta.account_id,
            nonce = delta.nonce,
            "Client-abandoned candidate discarded; account released"
        );

        Ok(())
    }

    /// Reset a candidate's persisted divergence streak after a read showed
    /// the account still at the candidate's base: divergence must be
    /// observed on *consecutive* ticks, so a non-diverged observation in
    /// between restarts the count. No-op (and no storage write) unless a
    /// streak was in progress.
    async fn reset_divergence_streak(&self, mut delta: DeltaObject) -> Result<DeltaObject> {
        if delta.status.divergence_count() == 0 {
            return Ok(delta);
        }

        tracing::info!(
            account_id = %delta.account_id,
            nonce = delta.nonce,
            divergence_count = delta.status.divergence_count(),
            "On-chain commitment back at candidate base; resetting divergence streak"
        );

        let new_status = delta.status.with_reset_divergence();
        let outcome = self
            .state
            .storage
            .update_candidate_status(
                &delta.account_id,
                delta.nonce,
                new_status.clone(),
                self.fence().as_ref(),
            )
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to update delta status: {e}"))
            })?;
        match outcome {
            CanonicalWrite::Applied => delta.status = new_status,
            CanonicalWrite::StaleLease => return Err(Self::stale_lease_error(&delta)),
            CanonicalWrite::NotCandidate => Self::log_not_candidate(&delta, "divergence_reset"),
        }

        Ok(delta)
    }

    /// Handle a candidate whose account moved past its base state on-chain.
    ///
    /// Discarding is gated on `divergence_confirmations` consecutive
    /// observations: a single read can come from a lagging RPC node whose
    /// stale commitment looks diverged while the candidate's transaction
    /// actually landed. The counter is persisted on the delta status so
    /// confirmation survives worker restarts.
    async fn handle_diverged_candidate(&self, delta: DeltaObject, on_chain: &str) -> Result<()> {
        let observations = delta.status.divergence_count() + 1;

        if observations < self.divergence_confirmations {
            tracing::info!(
                account_id = %delta.account_id,
                nonce = delta.nonce,
                on_chain = %on_chain,
                prev_commitment = %delta.prev_commitment,
                observations,
                divergence_confirmations = self.divergence_confirmations,
                "On-chain commitment matches neither candidate base nor expected state; \
                 deferring discard until divergence is confirmed"
            );

            let new_status = delta.status.with_incremented_divergence();
            let outcome = self
                .state
                .storage
                .update_candidate_status(
                    &delta.account_id,
                    delta.nonce,
                    new_status,
                    self.fence().as_ref(),
                )
                .await
                .map_err(|e| {
                    GuardianError::StorageError(format!("Failed to update delta status: {e}"))
                })?;
            match outcome {
                CanonicalWrite::Applied => record_candidate_outcome(
                    crate::metrics::labels::CandidateOutcome::DivergenceDeferred,
                ),
                CanonicalWrite::StaleLease => return Err(Self::stale_lease_error(&delta)),
                CanonicalWrite::NotCandidate => {
                    Self::log_not_candidate(&delta, "divergence_increment")
                }
            }

            return Ok(());
        }

        let now = self.state.clock.now().to_rfc3339();

        // A client abandon intent resolves here instead of being wiped
        // into a retained status (whose write drops the annotation): a
        // confirmed divergence is exactly the "this transaction can
        // never land anymore" evidence the abandon quarantine waits
        // for, so the abandon finalizes immediately — the delta becomes
        // `Discarded { client_abandoned }` history and the account is
        // released, which is what the client asked for.
        if delta.status.abandon_requested_at().is_some() {
            return self.finalize_abandoned_candidate(delta).await;
        }

        // A confirmed divergence is still only an observation, and it
        // has been wrong before: the pre-#326 empty-digest misread
        // condemned new accounts' first transactions as diverged while
        // they were still proving, and a consistently lagging RPC node
        // can beat the confirmation streak the same way. So the
        // candidate is retained-and-labeled (issue #345), not deleted:
        // the account is released either way, a spurious verdict heals
        // through reconciliation, and a genuine one ages out at the TTL
        // (monotonicity guarantees it can never start matching).
        if self.retained_ttl_seconds > 0 {
            tracing::warn!(
                account_id = %delta.account_id,
                nonce = delta.nonce,
                on_chain = %on_chain,
                prev_commitment = %delta.prev_commitment,
                observations,
                "Account advanced past candidate's base state on-chain; \
                 retaining the candidate for background reconciliation \
                 and releasing the account"
            );

            match self
                .retain_candidate(&delta, RetainReason::Diverged, &now)
                .await?
            {
                CanonicalWrite::Applied => {
                    record_candidate_outcome(crate::metrics::labels::CandidateOutcome::Diverged);
                }
                CanonicalWrite::StaleLease => return Err(Self::stale_lease_error(&delta)),
                CanonicalWrite::NotCandidate => Self::log_not_candidate(&delta, "diverged_retain"),
            }

            return Ok(());
        }

        tracing::warn!(
            account_id = %delta.account_id,
            nonce = delta.nonce,
            on_chain = %on_chain,
            prev_commitment = %delta.prev_commitment,
            observations,
            "Account advanced past candidate's base state on-chain; discarding \
             unsatisfiable candidate and releasing the account"
        );

        match self
            .remove_delta(&delta, DeltaStatusKind::Candidate, &now)
            .await?
        {
            CanonicalWrite::Applied => {
                record_candidate_outcome(crate::metrics::labels::CandidateOutcome::Diverged);
            }
            CanonicalWrite::StaleLease => return Err(Self::stale_lease_error(&delta)),
            CanonicalWrite::NotCandidate => Self::log_not_candidate(&delta, "diverged_discard"),
        }

        Ok(())
    }

    /// Handle a candidate whose expected state was not (yet) observed
    /// on-chain: defer within the submission grace period, then consume
    /// retry budget and discard once exhausted.
    async fn handle_unverified_candidate(&self, delta: DeltaObject, reason: &str) -> Result<()> {
        let now = self.state.clock.now();
        if let Some(candidate_age_seconds) = self.candidate_age_seconds(&delta, now)
            && candidate_age_seconds < self.submission_grace_period_seconds
        {
            tracing::info!(
                account_id = %delta.account_id,
                nonce = delta.nonce,
                candidate_age_seconds,
                submission_grace_period_seconds = self.submission_grace_period_seconds,
                error = %reason,
                "Delta verification failed during submission grace period; will retry without consuming retry budget"
            );
            record_candidate_outcome(crate::metrics::labels::CandidateOutcome::GraceDeferred);

            return Ok(());
        }

        let current_retry = delta.status.retry_count();
        let new_retry = current_retry + 1;
        let now = now.to_rfc3339();

        if new_retry >= self.max_retries {
            // An abandon-requested candidate resolves as
            // `Discarded { client_abandoned }` — queryable history, same
            // as the divergence path — instead of the historical delete
            // (which left the client's status poll on `not found`).
            // Finalizing without at-base confirmation is safe here
            // because the reconcile net backstops it: a transaction
            // that did land is promoted later — landed always wins.
            if delta.status.abandon_requested_at().is_some() {
                return self.finalize_abandoned_candidate(delta).await;
            }

            // Retry exhaustion never proved the delta wrong — verification
            // just never observed its expected commitment (RPC outage,
            // worker downtime, slow proving). Keep it as `retained`
            // (issue #345) so the reconcile pass can promote it if the
            // chain ever shows it landed; deleting it here is what left
            // stored state permanently behind the on-chain nonce.
            if self.retained_ttl_seconds > 0 {
                tracing::warn!(
                    account_id = %delta.account_id,
                    nonce = delta.nonce,
                    retries = new_retry,
                    max_retries = self.max_retries,
                    error = %reason,
                    "Delta verification failed after max retries; retaining \
                     for background reconciliation and releasing the account"
                );

                match self
                    .retain_candidate(&delta, RetainReason::RetryExhausted, &now)
                    .await?
                {
                    CanonicalWrite::Applied => {
                        record_candidate_outcome(
                            crate::metrics::labels::CandidateOutcome::Retained,
                        );
                    }
                    CanonicalWrite::StaleLease => return Err(Self::stale_lease_error(&delta)),
                    CanonicalWrite::NotCandidate => {
                        Self::log_not_candidate(&delta, "retain");
                    }
                }
                return Ok(());
            }

            tracing::warn!(
                account_id = %delta.account_id,
                nonce = delta.nonce,
                retries = new_retry,
                max_retries = self.max_retries,
                error = %reason,
                "Delta verification failed after max retries, discarding"
            );

            match self
                .remove_delta(&delta, DeltaStatusKind::Candidate, &now)
                .await?
            {
                CanonicalWrite::Applied => {
                    record_candidate_outcome(crate::metrics::labels::CandidateOutcome::Discarded);
                }
                CanonicalWrite::StaleLease => return Err(Self::stale_lease_error(&delta)),
                CanonicalWrite::NotCandidate => {
                    Self::log_not_candidate(&delta, "retry_discard");
                }
            }
        } else {
            tracing::info!(
                account_id = %delta.account_id,
                nonce = delta.nonce,
                retry = new_retry,
                max_retries = self.max_retries,
                error = %reason,
                "Delta verification failed, will retry"
            );

            let new_status = delta.status.with_incremented_retry(now);

            let outcome = self
                .state
                .storage
                .update_candidate_status(
                    &delta.account_id,
                    delta.nonce,
                    new_status,
                    self.fence().as_ref(),
                )
                .await
                .map_err(|e| {
                    GuardianError::StorageError(format!("Failed to update delta status: {e}"))
                })?;
            match outcome {
                CanonicalWrite::Applied => {
                    record_candidate_outcome(crate::metrics::labels::CandidateOutcome::Retried);
                    metrics::counter!(crate::metrics::names::CANONICALIZATION_RETRIES_TOTAL)
                        .increment(1);
                }
                CanonicalWrite::StaleLease => return Err(Self::stale_lease_error(&delta)),
                CanonicalWrite::NotCandidate => {
                    Self::log_not_candidate(&delta, "retry_increment");
                }
            }
        }

        Ok(())
    }

    /// Delete a delta that can never be canonicalized, delete its
    /// matching proposal, and release the account's pending-candidate lock.
    /// The delete, the fence validation, and the conditional flag clear
    /// commit as one fenced storage write; the delete only touches a row
    /// still in the expected lifecycle `kind` (candidate for the classic
    /// discard, retained for the issue #345 TTL expiry), so a stale
    /// discard can never remove a delta another owner promoted to
    /// canonical.
    async fn remove_delta(
        &self,
        delta: &DeltaObject,
        kind: DeltaStatusKind,
        now: &str,
    ) -> Result<CanonicalWrite> {
        let storage_backend = self.state.storage.clone();

        let outcome = storage_backend
            .discard_candidate(
                self.state.metadata.as_ref(),
                &delta.account_id,
                delta.nonce,
                kind,
                now,
                self.fence().as_ref(),
            )
            .await
            .map_err(|e| GuardianError::StorageError(format!("Failed to delete delta: {e}")))?;
        if outcome != CanonicalWrite::Applied {
            return Ok(outcome);
        }

        // A discarded candidate can never be canonicalized, so delete its proposal:
        // leaving it would strand it as `pending` forever and let clients re-submit a
        // stale intent. Cleanup follows the committed write — a refused write means
        // another owner finished this delta and its cleanup is not ours to run.
        let _ = self.delete_matching_proposal(delta).await;

        Ok(CanonicalWrite::Applied)
    }

    /// Deletes the multisig proposal matching a delta that is leaving
    /// the active candidate path (discarded, expired, or retained).
    /// Returns `true` when no proposal remains (deleted, or none found);
    /// `false` when a proposal likely still exists but could not be
    /// removed, so the caller must defer its status transition and retry
    /// next pass. An underivable proposal id is a permanent condition —
    /// it only warns and reports `true`, since deferring would wedge the
    /// delta forever without ever fixing the proposal.
    async fn delete_matching_proposal(&self, delta: &DeltaObject) -> bool {
        let storage_backend = self.state.storage.clone();

        let id = {
            let client = &self.state.network_client;
            match client.delta_proposal_id(&delta.account_id, delta.nonce, &delta.delta_payload) {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!(
                        account_id = %delta.account_id,
                        nonce = delta.nonce,
                        error = %e,
                        "Could not derive proposal id; \
                         its proposal may remain stranded as pending"
                    );
                    return true;
                }
            }
        };
        match storage_backend
            .pull_delta_proposal(&delta.account_id, &id)
            .await
        {
            Ok(_existing_proposal) => {
                tracing::warn!(
                    account_id = %delta.account_id,
                    proposal_id = %id,
                    "Deleting matching proposal as its delta left the candidate path"
                );
                if let Err(e) = storage_backend
                    .delete_delta_proposal(&delta.account_id, &id)
                    .await
                {
                    tracing::warn!(
                        account_id = %delta.account_id,
                        proposal_id = %id,
                        error = %e,
                        "Failed to delete proposal; deferring the delta's transition"
                    );
                    return false;
                }
                true
            }
            // Absence is the normal case (no proposal, or already
            // cleaned); only an actual read failure defers the
            // transition, since a proposal may still exist behind it.
            Err(e) if crate::storage::is_storage_not_found(&e) => true,
            Err(e) => {
                tracing::warn!(
                    account_id = %delta.account_id,
                    proposal_id = %id,
                    error = %e,
                    "Could not check for a matching proposal; deferring the delta's transition"
                );
                false
            }
        }
    }

    /// Keep a retry-exhausted candidate as `retained` (issue #345) and
    /// release the account: the status flip drops the row out of the
    /// pending-candidate lock scan, and the conditional flag clear
    /// re-checks the delta store so a candidate committed concurrently
    /// is never masked. The matching proposal is deleted here: the delta
    /// row itself carries everything reconciliation needs, and a
    /// proposal left `pending` would be stranded the moment a
    /// resubmission supersedes the retained row. Cleanup follows the
    /// committed status flip — running it first would let a stale worker
    /// delete the proposal of a same-payload resubmission that already
    /// superseded this row (the write refusal is what reveals that). A
    /// failed cleanup is therefore retried by the reconcile pass, which
    /// re-attempts proposal cleanup for every retained row it visits.
    async fn retain_candidate(
        &self,
        delta: &DeltaObject,
        reason: RetainReason,
        now: &str,
    ) -> Result<CanonicalWrite> {
        let outcome = self
            .state
            .storage
            .update_candidate_status(
                &delta.account_id,
                delta.nonce,
                DeltaStatus::retained(now.to_string(), reason),
                self.fence().as_ref(),
            )
            .await
            .map_err(|e| GuardianError::StorageError(format!("Failed to retain delta: {e}")))?;
        if outcome != CanonicalWrite::Applied {
            return Ok(outcome);
        }

        let expires_at = DateTime::parse_from_rfc3339(now)
            .ok()
            .map(|at| at.with_timezone(&Utc))
            .and_then(|at| {
                i64::try_from(self.retained_ttl_seconds)
                    .ok()
                    .and_then(chrono::Duration::try_seconds)
                    .and_then(|ttl| at.checked_add_signed(ttl))
            })
            .map(|at| at.to_rfc3339());
        tracing::info!(
            event = "candidate_retained",
            reason = match reason {
                RetainReason::RetryExhausted => "retry_exhausted",
                RetainReason::Diverged => "diverged",
            },
            account_id = %delta.account_id,
            nonce = delta.nonce,
            expires_at = expires_at.as_deref().unwrap_or("unbounded"),
            "Candidate parked as retained; account released, background \
             reconciliation takes over"
        );

        if let Err(e) = self
            .state
            .metadata
            .clear_pending_candidate_if_none(&delta.account_id, now)
            .await
        {
            tracing::warn!(
                account_id = %delta.account_id,
                error = %e,
                "Failed to clear has_pending_candidate flag after retaining; \
                 the stale-flag heal clears it on a later run"
            );
        }

        let _ = self.delete_matching_proposal(delta).await;

        Ok(CanonicalWrite::Applied)
    }

    async fn canonicalize_verified_delta(
        &self,
        delta: DeltaObject,
        new_state_json: serde_json::Value,
        verified_commitment: String,
        success_outcome: crate::metrics::labels::CandidateOutcome,
    ) -> Result<()> {
        tracing::info!(
            account_id = %delta.account_id,
            nonce = delta.nonce,
            "Canonicalizing delta (commitment matches on-chain)"
        );

        let account_metadata = self
            .state
            .metadata
            .get(&delta.account_id)
            .await
            .map_err(|e| GuardianError::StorageError(format!("Failed to get metadata: {e}")))?
            .ok_or_else(|| GuardianError::AccountNotFound(delta.account_id.clone()))?;

        let storage_backend = self.state.storage.clone();

        let current_state = storage_backend
            .pull_state(&delta.account_id)
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to get current state: {e}"))
            })?;

        let now = self.state.clock.now_rfc3339();

        let updated_state = StateObject {
            account_id: delta.account_id.clone(),
            state_json: new_state_json.clone(),
            commitment: verified_commitment,
            created_at: current_state.created_at.clone(),
            updated_at: now.clone(),
            auth_scheme: String::new(),
        };

        let new_auth = {
            let client = &self.state.network_client;
            client
                .should_update_auth(&new_state_json, &account_metadata.auth)
                .await
                .map_err(|e| {
                    GuardianError::StorageError(format!("Failed to check auth update: {e}"))
                })?
        };
        if new_auth.is_some() {
            tracing::debug!(
                account_id = %delta.account_id,
                "Syncing cosigner public keys from on-chain storage"
            );
        }

        // The typed `metadata` blob is populated at push time. The
        // commitment is the verified one: a missing or mismatched client
        // claim must not survive on the canonical record while the state
        // row carries the value verification proved on-chain.
        let mut canonical_delta = delta.clone();
        canonical_delta.new_commitment = Some(updated_state.commitment.clone());
        canonical_delta.status = DeltaStatus::canonical(now.clone());

        // State, auth, delta status, and the pending-candidate flag commit
        // as one fenced storage write: a crash, outage, or lease loss can
        // never advance the state while the delta stays a candidate.
        let outcome = storage_backend
            .promote_candidate(
                self.state.metadata.as_ref(),
                CandidatePromotion {
                    state: updated_state.clone(),
                    delta: canonical_delta,
                    new_auth,
                    now: now.clone(),
                    fence: self.fence(),
                    // Derived from the status the verification read: the
                    // write must only flip a row still in that exact
                    // lifecycle kind (see `PromotableKind`).
                    source: crate::storage::PromotableKind::of(&delta.status),
                },
            )
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to canonicalize delta: {e}"))
            })?;
        match outcome {
            PromoteWrite::Applied => {}
            PromoteWrite::StaleLease => return Err(Self::stale_lease_error(&delta)),
            PromoteWrite::NotCandidate => {
                Self::log_not_candidate(&delta, "promote");
                return Ok(());
            }
            PromoteWrite::StaleBase => {
                tracing::warn!(
                    account_id = %delta.account_id,
                    nonce = delta.nonce,
                    prev_commitment = %delta.prev_commitment,
                    "Stored state moved off the candidate's base during the pass; \
                     promotion rolled back, next pass re-verifies against the new base"
                );
                record_candidate_outcome(crate::metrics::labels::CandidateOutcome::StaleBase);
                return Ok(());
            }
        }

        let proposal_id = {
            let client = &self.state.network_client;
            client
                .delta_proposal_id(&delta.account_id, delta.nonce, &delta.delta_payload)
                .ok()
        };

        if let Some(ref id) = proposal_id
            && let Ok(_existing_proposal) = storage_backend
                .pull_delta_proposal(&delta.account_id, id)
                .await
        {
            tracing::info!(
                account_id = %delta.account_id,
                proposal_id = %id,
                "Deleting matching proposal as delta is now canonical"
            );
            // The proposal is finalized the moment its delta became
            // canonical with a matching proposal found — the delete
            // below is cleanup, so the event counts regardless of the
            // delete outcome (failures stay visible via
            // storage_operations_total{operation="delete_delta_proposal"}).
            metrics::counter!(
                crate::metrics::names::PROPOSALS_TOTAL,
                crate::metrics::names::LABEL_EVENT =>
                    crate::metrics::labels::ProposalEvent::Finalized.as_str()
            )
            .increment(1);
            if let Err(e) = storage_backend
                .delete_delta_proposal(&delta.account_id, id)
                .await
            {
                tracing::warn!(
                    account_id = %delta.account_id,
                    proposal_id = %id,
                    error = %e,
                    "Failed to delete proposal, but continuing"
                );
            }
        }

        // Issue #305: if this canonicalized delta moved the account's
        // guardian key away from this server (a SwitchGuardian pushed to
        // the pre-switch guardian), release the account. Best-effort —
        // the delta is already canonical either way.
        crate::services::release_on_switch::release_if_guardian_switched(
            &self.state,
            &account_metadata,
            &new_state_json,
            delta.nonce,
            &updated_state.commitment,
        )
        .await;

        record_candidate_outcome(success_outcome);
        Ok(())
    }
}

pub struct DeltasProcessor {
    base: DeltasProcessorBase,
}

impl DeltasProcessor {
    /// Single-process processor (filesystem / tests): always the leader, never
    /// fenced out. Behavior is identical to the pre-lease worker.
    #[allow(dead_code)]
    pub fn new(state: AppState, config: CanonicalizationConfig) -> Self {
        Self::new_with_mode(state, config, ProcessingMode::Full)
    }

    fn new_with_mode(
        state: AppState,
        config: CanonicalizationConfig,
        mode: ProcessingMode,
    ) -> Self {
        let pass = PassLease::single_process();
        Self::with_lease_and_mode(state, config, pass.leader, pass.lease, pass.cancel, mode)
    }

    pub(super) fn with_lease_and_mode(
        state: AppState,
        config: CanonicalizationConfig,
        leader: Arc<dyn LeaderElector>,
        lease: Lease,
        cancel: CancellationToken,
        mode: ProcessingMode,
    ) -> Self {
        Self::with_controls(
            state,
            config,
            leader,
            lease,
            cancel,
            mode,
            PassControls::unbounded(),
        )
    }

    pub(super) fn with_controls(
        state: AppState,
        config: CanonicalizationConfig,
        leader: Arc<dyn LeaderElector>,
        lease: Lease,
        cancel: CancellationToken,
        mode: ProcessingMode,
        controls: PassControls,
    ) -> Self {
        Self {
            base: DeltasProcessorBase {
                state,
                pass: PassLease {
                    leader,
                    lease,
                    cancel,
                },
                mode,
                max_retries: config.max_retries,
                submission_grace_period_seconds: config.submission_grace_period_seconds,
                divergence_confirmations: config.divergence_confirmations,
                abandon_quarantine_seconds: config.abandon_quarantine_seconds,
                abandon_quarantine_checks: config.abandon_quarantine_checks,
                max_concurrent_accounts: config.max_concurrent_accounts,
                retained_ttl_seconds: config.retained_ttl_seconds,
                reconcile_interval_seconds: config.reconcile_interval_seconds,
                reconcile_page_size: config.reconcile_page_size,
                reconcile_backoff: controls.reconcile_backoff,
                fast_promotion_state: controls.fast_state,
                reconcile_state: controls.reconcile_state,
                pass_deadline: controls.deadline,
            },
        }
    }
}

#[async_trait]
impl Processor for DeltasProcessor {
    async fn process_all_accounts(&self) -> Result<PassSummary> {
        self.base.process_all_accounts().await
    }

    async fn process_account(&self, account_id: &str) -> Result<()> {
        self.base.process_account(account_id).await
    }
}

pub struct TestDeltasProcessor {
    base: DeltasProcessorBase,
}

impl TestDeltasProcessor {
    pub fn new(state: AppState) -> Self {
        Self {
            base: DeltasProcessorBase {
                state,
                pass: PassLease::single_process(),
                mode: ProcessingMode::Full,
                max_retries: u32::MAX, // Test processor doesn't discard on retries
                submission_grace_period_seconds: 0,
                divergence_confirmations: u32::MAX, // ...nor on divergence
                abandon_quarantine_seconds: 0,      // ...and resolves abandons immediately
                abandon_quarantine_checks: 1,
                max_concurrent_accounts: 1, // ...and stays deterministic
                retained_ttl_seconds: u64::MAX, // ...nor expires retained deltas
                reconcile_interval_seconds: 1,
                reconcile_page_size: u32::MAX, // ...and reconciles everything at once
                reconcile_backoff: false,      // ...immediately, every run
                fast_promotion_state: Arc::new(FastPromotionState::default()),
                reconcile_state: Arc::new(ReconcileState::default()),
                pass_deadline: None,
            },
        }
    }
}

#[async_trait]
impl Processor for TestDeltasProcessor {
    /// Process-now semantics for tests, demos and e2e endpoints: one call
    /// runs a full candidate pass AND a reconcile pass, so callers that
    /// previously observed reconciliation inside the full pass still do.
    async fn process_all_accounts(&self) -> Result<PassSummary> {
        let full = self.base.process_full_pass().await?;
        let reconcile = self.base.process_reconcile_pass().await?;
        Ok(PassSummary {
            accounts: full.accounts + reconcile.accounts,
            failed_accounts: full.failed_accounts + reconcile.failed_accounts,
            cancelled: full.cancelled || reconcile.cancelled,
        })
    }

    async fn process_account(&self, account_id: &str) -> Result<()> {
        self.base.process_account(account_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::test::MockClock;
    use crate::delta_object::DeltaStatus;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::state_object::StateObject;
    use crate::testing::helpers::create_test_app_state_with_mocks;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;

    fn create_test_metadata(account_id: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: true,
            last_auth_timestamp: None,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    fn create_test_state(account_id: &str) -> StateObject {
        StateObject {
            account_id: account_id.to_string(),
            commitment: "prev_commitment".to_string(),
            state_json: serde_json::json!({"balance": 100}),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            auth_scheme: String::new(),
        }
    }

    fn create_candidate_delta(account_id: &str, nonce: u64) -> DeltaObject {
        DeltaObject {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: "prev_commitment".to_string(),
            new_commitment: Some("new_commitment".to_string()),
            delta_payload: serde_json::json!({"test": "payload"}),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::candidate("2024-01-01T00:00:00Z".to_string()),
            metadata: None,
        }
    }

    fn create_test_app_state_with_clock(
        storage: Arc<dyn crate::storage::StorageBackend>,
        network_client: Arc<dyn crate::network::NetworkClient>,
        metadata: Arc<dyn crate::metadata::MetadataStore>,
        clock: Arc<dyn crate::clock::Clock>,
    ) -> AppState {
        let mut state = create_test_app_state_with_mocks(storage, network_client, metadata);
        state.clock = clock;
        state
    }

    #[tokio::test]
    async fn processor_consumes_the_candidate_filtered_read() {
        // The pass must go through `pull_candidate_deltas` (the
        // store-side filter), not re-fetch the full history: only the
        // filtered queue is populated here, and the promotion still runs.
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_candidate_deltas(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_submit_state(Ok(()))
                .with_submit_delta(Ok(())),
        );

        let mock_network = Arc::new(
            MockNetworkClient::new()
                .with_apply_delta(Ok((
                    serde_json::json!({"new": "state"}),
                    "new_commitment".to_string(),
                )))
                .with_verify_commitment(Ok(StateVerification::Match))
                .with_should_update_auth(Ok(None)),
        );

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(()));

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            mock_network.clone(),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
        assert_eq!(storage.get_submit_state_calls().len(), 1);
        assert_eq!(
            mock_network.get_verify_commitment_calls(),
            vec![(account_id.to_string(), "new_commitment".to_string())],
        );
    }

    #[tokio::test]
    async fn promotion_only_pass_promotes_recent_verified_candidate() {
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);
        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_recent_candidate_deltas(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_promote_candidate(Ok(PromoteWrite::Applied)),
        );
        let network = Arc::new(
            MockNetworkClient::new()
                .with_apply_delta(Ok((
                    serde_json::json!({"new": "state"}),
                    "new_commitment".to_string(),
                )))
                .with_verify_commitment(Ok(StateVerification::Match))
                .with_should_update_auth(Ok(None)),
        );
        let metadata = Arc::new(
            MockMetadataStore::new()
                .with_get(Ok(Some(create_test_metadata(account_id))))
                .with_get(Ok(Some(create_test_metadata(account_id)))),
        );
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state =
            create_test_app_state_with_clock(storage.clone(), network.clone(), metadata, clock);
        let processor = DeltasProcessor::new_with_mode(
            state,
            CanonicalizationConfig::default(),
            ProcessingMode::PromoteRecent {
                max_age_seconds: 30,
            },
        );

        let result = processor.process_all_accounts().await;

        assert!(result.is_ok());
        assert_eq!(storage.get_promote_candidate_fences(), vec![None]);
        assert_eq!(
            storage.get_pull_recent_candidate_deltas_calls(),
            vec![(
                Utc.with_ymd_and_hms(2023, 12, 31, 23, 59, 35).unwrap(),
                None,
                FAST_PROMOTION_PAGE_SIZE,
            )]
        );
    }

    #[tokio::test]
    async fn promotion_only_pass_does_not_advance_divergence_or_discard() {
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status = DeltaStatus::Candidate {
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            retry_count: 17,
            divergence_count: 1,
            abandon_requested_at: None,
            abandon_confirm_count: 0,
        };
        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_recent_candidate_deltas(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );
        let network = Arc::new(
            MockNetworkClient::new()
                .with_apply_delta(Ok((
                    serde_json::json!({"new": "state"}),
                    "new_commitment".to_string(),
                )))
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: "0xdiverged".to_string(),
                })),
        );
        let metadata =
            Arc::new(MockMetadataStore::new().with_get(Ok(Some(create_test_metadata(account_id)))));
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state =
            create_test_app_state_with_clock(storage.clone(), network.clone(), metadata, clock);
        let processor = DeltasProcessor::new_with_mode(
            state,
            CanonicalizationConfig::default(),
            ProcessingMode::PromoteRecent {
                max_age_seconds: 30,
            },
        );

        let result = processor.process_all_accounts().await;

        assert!(result.is_ok());
        assert!(storage.get_update_delta_status_calls().is_empty());
        assert!(storage.get_delete_delta_calls().is_empty());
        assert_eq!(network.apply_delta_responses.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn promotion_only_pass_requires_reconstructed_commitment_to_match_claim() {
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);
        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_recent_candidate_deltas(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );
        let network = Arc::new(
            MockNetworkClient::new()
                .with_verify_commitment(Ok(StateVerification::Match))
                .with_apply_delta(Ok((
                    serde_json::json!({"new": "state"}),
                    "different_commitment".to_string(),
                ))),
        );
        let metadata =
            Arc::new(MockMetadataStore::new().with_get(Ok(Some(create_test_metadata(account_id)))));
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(storage.clone(), network, metadata, clock);
        let processor = DeltasProcessor::new_with_mode(
            state,
            CanonicalizationConfig::default(),
            ProcessingMode::PromoteRecent {
                max_age_seconds: 30,
            },
        );

        let result = processor.process_all_accounts().await;

        assert!(result.is_ok());
        assert!(storage.get_promote_candidate_fences().is_empty());
    }

    #[tokio::test]
    async fn promotion_only_pass_pages_through_recent_candidates() {
        let account_id = "0xtest_account";
        let first_page = (1..=FAST_PROMOTION_PAGE_SIZE)
            .map(|nonce| {
                let mut candidate = create_candidate_delta(account_id, u64::from(nonce));
                candidate.new_commitment = None;
                candidate
            })
            .collect::<Vec<_>>();
        let mut final_candidate =
            create_candidate_delta(account_id, u64::from(FAST_PROMOTION_PAGE_SIZE) + 1);
        final_candidate.new_commitment = None;
        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_recent_candidate_deltas(Ok(vec![final_candidate]))
                .with_pull_recent_candidate_deltas(Ok(first_page)),
        );
        let metadata = Arc::new(
            MockMetadataStore::new()
                .with_get(Ok(Some(create_test_metadata(account_id))))
                .with_get(Ok(Some(create_test_metadata(account_id)))),
        );
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            Arc::new(MockNetworkClient::new()),
            metadata,
            clock,
        );
        let processor = DeltasProcessor::new_with_mode(
            state,
            CanonicalizationConfig::default(),
            ProcessingMode::PromoteRecent {
                max_age_seconds: 30,
            },
        );

        let result = processor.process_all_accounts().await;

        assert!(result.is_ok());
        let calls = storage.get_pull_recent_candidate_deltas_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1, None);
        assert_eq!(
            calls[1].1,
            Some(RecentCandidateCursor {
                last_status_timestamp: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
                last_account_id: account_id.to_string(),
                last_nonce: u64::from(FAST_PROMOTION_PAGE_SIZE),
            })
        );
    }

    #[tokio::test]
    async fn promotion_only_pass_ends_pagination_on_malformed_cursor_timestamp() {
        let account_id = "0xtest_account";
        let page = (1..=FAST_PROMOTION_PAGE_SIZE)
            .map(|nonce| {
                let mut candidate = create_candidate_delta(account_id, u64::from(nonce));
                candidate.new_commitment = None;
                candidate
            })
            .map(|mut candidate| {
                if candidate.nonce == u64::from(FAST_PROMOTION_PAGE_SIZE) {
                    candidate.status = DeltaStatus::Candidate {
                        timestamp: "not-a-timestamp".to_string(),
                        retry_count: 0,
                        divergence_count: 0,
                        abandon_requested_at: None,
                        abandon_confirm_count: 0,
                    };
                }
                candidate
            })
            .collect::<Vec<_>>();
        let storage =
            Arc::new(MockStorageBackend::new().with_pull_recent_candidate_deltas(Ok(page)));
        let metadata =
            Arc::new(MockMetadataStore::new().with_get(Ok(Some(create_test_metadata(account_id)))));
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            Arc::new(MockNetworkClient::new()),
            metadata,
            clock,
        );
        let processor = DeltasProcessor::new_with_mode(
            state,
            CanonicalizationConfig::default(),
            ProcessingMode::PromoteRecent {
                max_age_seconds: 30,
            },
        );

        let result = processor.process_all_accounts().await;

        assert!(result.is_ok());
        assert_eq!(storage.get_pull_recent_candidate_deltas_calls().len(), 1);
    }

    /// Trips the pass's cancellation token from inside page processing —
    /// the deterministic stand-in for a deadline that expires while a
    /// page is being worked, which a wall-clock deadline cannot do
    /// reliably in a test.
    struct CancelOnVerifyNetwork {
        cancel: CancellationToken,
    }

    #[async_trait]
    impl crate::network::NetworkClient for CancelOnVerifyNetwork {
        fn get_state_commitment(
            &self,
            _account_id: &str,
            _state_json: &serde_json::Value,
        ) -> std::result::Result<String, String> {
            unreachable!()
        }

        async fn verify_commitment(
            &self,
            _account_id: &str,
            _expected_commitment: &str,
        ) -> std::result::Result<StateVerification, String> {
            self.cancel.cancel();
            Ok(StateVerification::Mismatch {
                on_chain: "0xother".to_string(),
            })
        }

        fn verify_delta(
            &self,
            _prev_proof: &str,
            _prev_state_json: &serde_json::Value,
            _delta_payload: &serde_json::Value,
        ) -> std::result::Result<(), String> {
            unreachable!()
        }

        fn apply_delta(
            &self,
            _prev_state_json: &serde_json::Value,
            _delta_payload: &serde_json::Value,
        ) -> std::result::Result<(serde_json::Value, String), String> {
            unreachable!()
        }

        fn merge_deltas(
            &self,
            _delta_payloads: Vec<serde_json::Value>,
        ) -> std::result::Result<serde_json::Value, String> {
            unreachable!()
        }

        fn delta_proposal_id(
            &self,
            _account_id: &str,
            _nonce: u64,
            _delta_payload: &serde_json::Value,
        ) -> std::result::Result<String, String> {
            unreachable!()
        }

        fn validate_account_id(&self, _account_id: &str) -> std::result::Result<(), String> {
            unreachable!()
        }

        fn validate_credential(
            &self,
            _state_json: &serde_json::Value,
            _credential: &crate::metadata::auth::Credentials,
            _auth: &Auth,
        ) -> std::result::Result<(), String> {
            unreachable!()
        }

        fn validate_guardian_commitment(
            &self,
            _state_json: &serde_json::Value,
            _expected_guardian_commitment: &str,
        ) -> std::result::Result<(), String> {
            unreachable!()
        }

        async fn should_update_auth(
            &self,
            _state_json: &serde_json::Value,
            _current_auth: &Auth,
        ) -> std::result::Result<Option<Auth>, String> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn promotion_only_pass_retains_cursor_when_pass_is_cut_short_after_page() {
        let account_id = "0xtest_account";
        let page = (1..=FAST_PROMOTION_PAGE_SIZE)
            .map(|nonce| create_candidate_delta(account_id, u64::from(nonce)))
            .collect::<Vec<_>>();
        let storage =
            Arc::new(MockStorageBackend::new().with_pull_recent_candidate_deltas(Ok(page)));
        let metadata =
            Arc::new(MockMetadataStore::new().with_get(Ok(Some(create_test_metadata(account_id)))));
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let pass = PassLease::single_process();
        let network = Arc::new(CancelOnVerifyNetwork {
            cancel: pass.cancel.clone(),
        });
        let state = create_test_app_state_with_clock(storage.clone(), network, metadata, clock);
        let fast_state = Arc::new(FastPromotionState::default());
        let processor = DeltasProcessor::with_controls(
            state,
            CanonicalizationConfig::default(),
            pass.leader,
            pass.lease,
            pass.cancel,
            ProcessingMode::PromoteRecent {
                max_age_seconds: 30,
            },
            PassControls {
                fast_state: fast_state.clone(),
                reconcile_state: Arc::new(ReconcileState::default()),
                deadline: None,
                reconcile_backoff: false,
            },
        );

        let result = processor.process_all_accounts().await;

        assert!(result.is_ok());
        assert_eq!(storage.get_pull_recent_candidate_deltas_calls().len(), 1);
        assert_eq!(
            fast_state.cursor(),
            Some(RecentCandidateCursor {
                last_status_timestamp: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
                last_account_id: account_id.to_string(),
                last_nonce: u64::from(FAST_PROMOTION_PAGE_SIZE),
            })
        );
    }

    #[tokio::test]
    async fn promotion_only_pass_stops_admitting_work_at_deadline() {
        let storage = Arc::new(MockStorageBackend::new());
        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(MockNetworkClient::new()),
            Arc::new(MockMetadataStore::new()),
        );
        let pass = PassLease::single_process();
        let processor = DeltasProcessor::with_controls(
            state,
            CanonicalizationConfig::default(),
            pass.leader,
            pass.lease,
            pass.cancel,
            ProcessingMode::PromoteRecent {
                max_age_seconds: 30,
            },
            PassControls {
                fast_state: Arc::new(FastPromotionState::default()),
                reconcile_state: Arc::new(ReconcileState::default()),
                deadline: Some(Instant::now()),
                reconcile_backoff: false,
            },
        );

        let result = processor.process_all_accounts().await;

        assert!(result.is_ok());
        assert!(storage.get_pull_recent_candidate_deltas_calls().is_empty());
    }

    #[tokio::test]
    async fn promotion_only_pass_skips_candidate_outside_fast_window() {
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);
        let storage = Arc::new(
            MockStorageBackend::new().with_pull_recent_candidate_deltas(Ok(vec![candidate])),
        );
        let network = Arc::new(MockNetworkClient::new());
        let metadata = Arc::new(MockMetadataStore::new());
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 30).unwrap(),
        ));
        let state = create_test_app_state_with_clock(storage, network.clone(), metadata, clock);
        let processor = DeltasProcessor::new_with_mode(
            state,
            CanonicalizationConfig::default(),
            ProcessingMode::PromoteRecent {
                max_age_seconds: 30,
            },
        );

        let result = processor.process_all_accounts().await;

        assert!(result.is_ok());
        assert!(network.get_verify_commitment_calls().is_empty());
    }

    #[tokio::test]
    async fn test_process_all_accounts_empty_list() {
        let mock_storage = MockStorageBackend::new();
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new().with_list_with_pending_candidates(Ok(vec![]));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_process_all_accounts_list_error() {
        let mock_storage = MockStorageBackend::new();
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Err("Database error".to_string()));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            GuardianError::StorageError(_)
        ));
    }

    #[tokio::test]
    async fn test_process_account_metadata_not_found() {
        let account_id = "0xtest_account";

        let mock_storage = MockStorageBackend::new();
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(None)); // Metadata not found

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        // process_all_accounts should continue even if one account fails,
        // and the pass summary must count the failure.
        let summary = processor
            .process_all_accounts()
            .await
            .expect("per-account failures do not fail the pass");
        assert_eq!(summary.accounts, 1);
        assert_eq!(summary.failed_accounts, 1);
        assert!(!summary.cancelled);
    }

    #[tokio::test]
    async fn test_process_account_no_candidates() {
        let account_id = "0xtest_account";

        let mock_storage = MockStorageBackend::new().with_pull_deltas_after(Ok(vec![])); // No deltas
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_process_account_clears_stale_pending_flag() {
        // Account is listed with a pending candidate and its metadata flag
        // is set, but no candidate delta exists (e.g. a best-effort
        // flag-clear failed after the delta was deleted). The worker must
        // heal the stale flag so the account leaves the scan list.
        let account_id = "0xtest_account";

        // The indexed candidate read returns nothing (mock default).
        let mock_storage = MockStorageBackend::new();
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata.clone()),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        processor.process_all_accounts().await.unwrap();

        let set_calls = mock_metadata.get_set_calls();
        assert_eq!(set_calls.len(), 1, "expected exactly one healing write");
        assert!(!set_calls[0].has_pending_candidate);
    }

    #[tokio::test]
    async fn test_process_account_no_candidates_and_clear_flag_skips_write() {
        // Same as above but the metadata flag is already clear (the account
        // reached the worker through a stale listing): no healing write.
        let account_id = "0xtest_account";

        let mut metadata_obj = create_test_metadata(account_id);
        metadata_obj.has_pending_candidate = false;

        let mock_storage = MockStorageBackend::new().with_pull_deltas_after(Ok(vec![]));
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(metadata_obj)));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata.clone()),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        processor.process_all_accounts().await.unwrap();

        assert!(mock_metadata.get_set_calls().is_empty());
    }

    #[tokio::test]
    async fn test_process_candidate_verification_succeeds() {
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);

        let mock_storage = MockStorageBackend::new()
            .with_pull_deltas_after(Ok(vec![candidate.clone()]))
            .with_pull_state(Ok(create_test_state(account_id)))
            .with_pull_state(Ok(create_test_state(account_id)))
            .with_pull_state(Ok(create_test_state(account_id)))
            .with_submit_state(Ok(()))
            .with_submit_delta(Ok(()));

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Match))
            .with_should_update_auth(Ok(None));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(())); // For clearing has_pending_candidate

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn stale_base_promotion_leaves_candidate_intact() {
        // The stored state moves off the candidate's base between the pass
        // reading it and the promotion write: the promotion must refuse,
        // leave the candidate and the pending flag untouched, and skip the
        // trailing proposal cleanup.
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);
        let mut moved_state = create_test_state(account_id);
        moved_state.commitment = "some_other_commitment".to_string();

        // LIFO queue: the moved state is pushed first so the promotion's
        // base check (third read) is the one that observes it.
        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(moved_state))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Match))
            .with_should_update_auth(Ok(None));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))));
        let metadata = Arc::new(mock_metadata);

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(mock_network),
            metadata.clone(),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        assert!(storage.get_submit_state_calls().is_empty());
        assert!(storage.get_delete_delta_calls().is_empty());
        assert!(storage.get_pull_delta_proposal_calls().is_empty());
        assert!(metadata.get_set_calls().is_empty());
    }

    #[tokio::test]
    async fn test_process_candidate_verification_fails_increments_retry() {
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);

        let mock_storage = MockStorageBackend::new()
            .with_pull_deltas_after(Ok(vec![candidate.clone()]))
            .with_pull_state(Ok(create_test_state(account_id)));

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Err("Verification failed".to_string()));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        // Use max_retries > 1 so it increments instead of discarding
        let config = CanonicalizationConfig::new(10, 18);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_process_candidate_within_submission_grace_does_not_discard() {
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status =
            DeltaStatus::candidate_with_retry("2024-01-01T00:00:00Z".to_string(), 17);

        let mock_storage = MockStorageBackend::new()
            .with_pull_deltas_after(Ok(vec![candidate]))
            .with_pull_state(Ok(create_test_state(account_id)));

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Err("Verification failed".to_string()));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))));
        let metadata = Arc::new(mock_metadata);

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            metadata.clone(),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18).with_submission_grace_period_seconds(30);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
        assert!(metadata.get_set_calls().is_empty());
    }

    #[tokio::test]
    async fn test_mismatch_at_candidate_base_defers_within_grace() {
        // On-chain still shows the candidate's prev_commitment: the tx has
        // not landed yet, so the grace period applies and nothing is written.
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "prev_commitment".to_string(),
            }));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))));
        let metadata = Arc::new(mock_metadata);

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            Arc::new(mock_network),
            metadata.clone(),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18).with_submission_grace_period_seconds(30);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
        assert!(storage.get_update_delta_status_calls().is_empty());
        assert!(storage.get_delete_delta_calls().is_empty());
        assert!(metadata.get_set_calls().is_empty());
    }

    #[tokio::test]
    async fn test_abandon_intent_confirms_and_bypasses_grace() {
        // A candidate with a recorded abandon intent, observed still at
        // its base INSIDE the submission grace period: the abandon
        // quarantine takes precedence over the grace deferral, so a
        // confirmation is persisted instead of nothing happening.
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status = candidate
            .status
            .with_abandon_requested("2024-01-01T00:00:00Z".to_string());

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "prev_commitment".to_string(),
            }));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))));
        let metadata = Arc::new(mock_metadata);

        // 5s after the request: age is far under the 600s grace AND under
        // the quarantine, and only one at-base observation exists — defer,
        // but persist the confirmation.
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            Arc::new(mock_network),
            metadata.clone(),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18)
            .with_submission_grace_period_seconds(600)
            .with_abandon_quarantine_seconds(30)
            .with_abandon_quarantine_checks(2);
        let processor = DeltasProcessor::new(state, config);

        processor.process_all_accounts().await.unwrap();

        let status_writes = storage.get_update_delta_status_calls();
        assert_eq!(
            status_writes.len(),
            1,
            "the abandon confirmation must be persisted despite the grace period"
        );
        let (_, _, written) = &status_writes[0];
        assert!(written.is_candidate(), "status must remain candidate");
        assert_eq!(written.abandon_confirm_count(), 1);
        assert_eq!(written.abandon_requested_at(), Some("2024-01-01T00:00:00Z"));
        assert!(storage.get_delete_delta_calls().is_empty());
    }

    #[tokio::test]
    async fn test_abandon_finalizes_after_quarantine() {
        // Quarantine satisfied: enough consecutive at-base confirmations
        // and enough wall time since the request. The delta transitions to
        // Discarded { reason: ClientAbandoned } — preserved as history —
        // and the pending-candidate flag is released.
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status = candidate
            .status
            .with_abandon_requested("2024-01-01T00:00:00Z".to_string())
            .with_incremented_abandon_confirm();

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "prev_commitment".to_string(),
            }));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))));
        let metadata = Arc::new(mock_metadata);

        // 60s after the request: second at-base observation, age >= 30s.
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 1, 0).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            Arc::new(mock_network),
            metadata.clone(),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18)
            .with_submission_grace_period_seconds(600)
            .with_abandon_quarantine_seconds(30)
            .with_abandon_quarantine_checks(2);
        let processor = DeltasProcessor::new(state, config);

        processor.process_all_accounts().await.unwrap();

        let status_writes = storage.get_update_delta_status_calls();
        assert_eq!(status_writes.len(), 1, "exactly the discard transition");
        let (_, _, written) = &status_writes[0];
        assert!(
            written.is_client_abandoned(),
            "delta must be discarded as client-abandoned, got {written:?}"
        );
        // History preserved, not deleted.
        assert!(storage.get_delete_delta_calls().is_empty());
        // The pending-candidate flag was released.
        let set_calls = metadata.get_set_calls();
        assert!(
            set_calls.iter().any(|m| !m.has_pending_candidate),
            "flag must be cleared after the abandon finalizes"
        );
    }

    #[tokio::test]
    async fn test_abandon_quarantine_waits_for_request_age() {
        // Confirmation count is satisfied but the request is too fresh:
        // the quarantine keeps waiting so a late-landing transaction can
        // still surface.
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status = candidate
            .status
            .with_abandon_requested("2024-01-01T00:00:00Z".to_string())
            .with_incremented_abandon_confirm();

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "prev_commitment".to_string(),
            }));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))));
        let metadata = Arc::new(mock_metadata);

        // Only 10s after the request: under the 30s quarantine.
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 10).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            Arc::new(mock_network),
            metadata.clone(),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18)
            .with_abandon_quarantine_seconds(30)
            .with_abandon_quarantine_checks(2);
        let processor = DeltasProcessor::new(state, config);

        processor.process_all_accounts().await.unwrap();

        let status_writes = storage.get_update_delta_status_calls();
        assert_eq!(status_writes.len(), 1);
        let (_, _, written) = &status_writes[0];
        assert!(written.is_candidate(), "must still be a candidate");
        assert_eq!(written.abandon_confirm_count(), 2);
    }

    #[tokio::test]
    async fn test_landed_candidate_canonicalizes_despite_abandon_intent() {
        // The transaction landed while the abandon was pending: the landed
        // outcome wins — the delta canonicalizes exactly as without intent.
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status = candidate
            .status
            .with_abandon_requested("2024-01-01T00:00:00Z".to_string());

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_submit_state(Ok(()))
                .with_submit_delta(Ok(())),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Match))
            .with_should_update_auth(Ok(None));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(()));
        let metadata = Arc::new(mock_metadata);

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 1, 0).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            Arc::new(mock_network),
            metadata.clone(),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18);
        let processor = DeltasProcessor::new(state, config);

        processor.process_all_accounts().await.unwrap();

        let submitted = storage.get_submit_delta_calls();
        assert!(
            submitted.iter().any(|d| d.status.is_canonical()),
            "the landed delta must canonicalize despite the abandon intent"
        );
    }

    #[tokio::test]
    async fn test_diverged_candidate_first_observation_defers() {
        // On-chain matches neither the candidate's base nor its expected new
        // commitment. A single observation could be a stale RPC read, so the
        // candidate is kept and only the persisted divergence counter grows —
        // grace period notwithstanding, no discard yet.
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "0xsome_other_commitment".to_string(),
            }));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))));
        let metadata = Arc::new(mock_metadata);

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            Arc::new(mock_network),
            metadata.clone(),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18).with_submission_grace_period_seconds(600);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        let status_updates = storage.get_update_delta_status_calls();
        assert_eq!(status_updates.len(), 1);
        assert_eq!(status_updates[0].2.divergence_count(), 1);
        assert_eq!(status_updates[0].2.retry_count(), 0);
        assert!(storage.get_delete_delta_calls().is_empty());
        assert!(metadata.get_set_calls().is_empty());
    }

    #[tokio::test]
    async fn test_diverged_candidate_confirmed_retains_and_releases() {
        // Second consecutive diverged observation reaches the default
        // confirmation threshold (2): the candidate is retained-and-labeled
        // (issue #345) — not deleted — and the account lock released
        // immediately, well within the grace period. Retention matters
        // because a diverged verdict has been spurious before (the
        // pre-#326 empty-digest misread of new accounts).
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status = DeltaStatus::Candidate {
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            retry_count: 0,
            divergence_count: 1,
            abandon_requested_at: None,
            abandon_confirm_count: 0,
        };

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate.clone()]))
                .with_pull_delta_proposal(Ok(candidate))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "0xsome_other_commitment".to_string(),
            }));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(()));
        let metadata = Arc::new(mock_metadata);

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            Arc::new(mock_network),
            metadata.clone(),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18).with_submission_grace_period_seconds(600);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        assert!(
            storage.get_delete_delta_calls().is_empty(),
            "a diverged candidate is retained, never deleted"
        );
        let status_writes = storage.get_update_delta_status_calls();
        assert_eq!(status_writes.len(), 1);
        assert!(status_writes[0].2.is_retained());
        assert_eq!(
            status_writes[0].2.retain_reason(),
            Some(crate::delta_object::RetainReason::Diverged)
        );
        assert_eq!(
            storage.delete_delta_proposal_calls.lock().unwrap().len(),
            1,
            "the matching proposal is deleted at retain time — left pending \
             it would be stranded forever once the row is superseded"
        );
        assert!(
            metadata
                .get_set_calls()
                .iter()
                .any(|m| !m.has_pending_candidate)
        );
    }

    #[tokio::test]
    async fn test_diverged_candidate_with_abandon_intent_finalizes_abandon() {
        // A confirmed divergence is exactly the "this transaction can
        // never land anymore" evidence an abandon request waits for, so
        // the intent resolves as Discarded{client_abandoned} instead of
        // being silently wiped into a retained status.
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status = DeltaStatus::Candidate {
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            retry_count: 0,
            divergence_count: 1,
            abandon_requested_at: Some("2024-01-01T00:00:01Z".to_string()),
            abandon_confirm_count: 0,
        };

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "0xsome_other_commitment".to_string(),
            }));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(()));

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18).with_submission_grace_period_seconds(600);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        assert!(storage.get_delete_delta_calls().is_empty());
        let status_writes = storage.get_update_delta_status_calls();
        assert_eq!(status_writes.len(), 1);
        assert!(
            status_writes[0].2.is_client_abandoned(),
            "the abandon intent resolves, it is not wiped into retention; got {:?}",
            status_writes[0].2
        );
    }

    #[tokio::test]
    async fn test_diverged_candidate_retention_disabled_discards() {
        // `retained_ttl_seconds = 0` restores the historical #312
        // behavior: a confirmed divergence deletes the candidate.
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status = DeltaStatus::Candidate {
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            retry_count: 0,
            divergence_count: 1,
            abandon_requested_at: None,
            abandon_confirm_count: 0,
        };

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "0xsome_other_commitment".to_string(),
            }));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(()));

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18)
            .with_submission_grace_period_seconds(600)
            .with_retained_ttl_seconds(0);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        assert_eq!(
            storage.get_delete_delta_calls(),
            vec![(account_id.to_string(), 1)]
        );
        assert!(storage.get_update_delta_status_calls().is_empty());
    }

    #[tokio::test]
    async fn test_divergence_confirmations_of_one_retains_immediately() {
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "0xsome_other_commitment".to_string(),
            }));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(()));
        let metadata = Arc::new(mock_metadata);

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            Arc::new(mock_network),
            metadata.clone(),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18)
            .with_submission_grace_period_seconds(600)
            .with_divergence_confirmations(1);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        assert!(storage.get_delete_delta_calls().is_empty());
        let status_writes = storage.get_update_delta_status_calls();
        assert_eq!(status_writes.len(), 1);
        assert!(status_writes[0].2.is_retained());
    }

    #[tokio::test]
    async fn test_absent_on_chain_account_is_not_discarded_as_diverged() {
        // A first transaction that has not landed yet verifies as `Absent`:
        // the network client reports the chain has never seen the account.
        // That is the not-yet-landed case, never divergence — the account
        // has not advanced past its base, it simply is not there yet. Even
        // with divergence_confirmations == 1 (which would discard a genuine
        // divergence on the first observation) the candidate must survive
        // and take the retry path.
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Absent));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(()));
        let metadata = Arc::new(mock_metadata);

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            Arc::new(mock_network),
            metadata.clone(),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18)
            .with_submission_grace_period_seconds(0)
            .with_divergence_confirmations(1);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        assert!(
            storage.get_delete_delta_calls().is_empty(),
            "an account absent from chain must not be discarded as diverged",
        );
        assert!(
            !storage.get_update_delta_status_calls().is_empty(),
            "the not-yet-landed candidate should take the retry path",
        );
    }

    #[tokio::test]
    async fn test_divergence_streak_resets_on_base_observation() {
        // diverged -> base -> diverged must NOT accumulate to the threshold:
        // the base read proves the earlier diverged read was stale, so the
        // streak restarts and the candidate survives all three ticks.
        let account_id = "0xtest_account";

        let fresh = create_candidate_delta(account_id, 1);
        let mut once_diverged = create_candidate_delta(account_id, 1);
        once_diverged.status = DeltaStatus::Candidate {
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            retry_count: 0,
            divergence_count: 1,
            abandon_requested_at: None,
            abandon_confirm_count: 0,
        };

        // Response queues are LIFO (`Vec::pop`), so tick 3's responses are
        // pushed first and tick 1's last. Ticks 2 and 3 pull the candidate
        // as the previous tick persisted it (the mock store is stateless).
        let storage = Arc::new(
            MockStorageBackend::new()
                // tick 3: streak was reset on tick 2
                .with_pull_deltas_after(Ok(vec![fresh.clone()]))
                .with_pull_state(Ok(create_test_state(account_id)))
                // tick 2: streak of 1 persisted by tick 1
                .with_pull_deltas_after(Ok(vec![once_diverged]))
                .with_pull_state(Ok(create_test_state(account_id)))
                // tick 1: fresh candidate
                .with_pull_deltas_after(Ok(vec![fresh]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            // tick 3: diverged again
            .with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "0xsome_other_commitment".to_string(),
            }))
            // tick 2: back at the candidate's base
            .with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "prev_commitment".to_string(),
            }))
            // tick 1: diverged
            .with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "0xsome_other_commitment".to_string(),
            }));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))));
        let metadata = Arc::new(mock_metadata);

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            Arc::new(mock_network),
            metadata.clone(),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18).with_submission_grace_period_seconds(600);
        let processor = DeltasProcessor::new(state, config);

        for _ in 0..3 {
            let result = processor.process_all_accounts().await;
            assert!(result.is_ok());
        }

        // Tick 1 starts a streak, tick 2 resets it, tick 3 starts over —
        // never reaching the threshold of 2.
        let observed: Vec<u32> = storage
            .get_update_delta_status_calls()
            .iter()
            .map(|(_, _, status)| status.divergence_count())
            .collect();
        assert_eq!(observed, vec![1, 0, 1]);
        assert!(storage.get_delete_delta_calls().is_empty());
        assert!(metadata.get_set_calls().is_empty());
    }

    #[tokio::test]
    async fn test_process_candidate_max_retries_discards() {
        let account_id = "0xtest_account";
        // Create a candidate that has already been retried max_retries times
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status = DeltaStatus::candidate_with_retry("2024-01-01T00:00:00Z".to_string(), 9);

        let mock_storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate.clone()]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Err("Verification failed".to_string()));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(())); // For clearing has_pending_candidate

        let state = create_test_app_state_with_mocks(
            mock_storage.clone(),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        // max_retries = 10 (retry_count 9 + 1 = 10 >= 10) with retention
        // disabled: the historical discard path deletes the delta.
        let config = CanonicalizationConfig::new(10, 10).with_retained_ttl_seconds(0);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
        assert_eq!(
            mock_storage.get_delete_delta_calls(),
            vec![(account_id.to_string(), 1)]
        );
    }

    fn create_retained_delta(account_id: &str, nonce: u64, retained_at: &str) -> DeltaObject {
        let mut delta = create_candidate_delta(account_id, nonce);
        delta.status = DeltaStatus::retained(
            retained_at.to_string(),
            crate::delta_object::RetainReason::RetryExhausted,
        );
        delta
    }

    /// Single-process processor running the dedicated reconcile pass
    /// (backoff disabled, like every non-worker construction).
    fn reconcile_processor(state: AppState, config: CanonicalizationConfig) -> DeltasProcessor {
        DeltasProcessor::new_with_mode(state, config, ProcessingMode::ReconcileRecoverable)
    }

    #[tokio::test]
    async fn test_max_retries_retains_candidate_for_reconciliation() {
        // Exhausting the retry budget parks the candidate as `retained`
        // (issue #345) instead of deleting it: the delta was never proven
        // wrong, and deleting it is what left stored state permanently
        // behind the on-chain nonce. The account lock is released and the
        // matching proposal is cleaned up (the delta row carries
        // everything a reconcile needs).
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status =
            DeltaStatus::candidate_with_retry("2024-01-01T00:00:00Z".to_string(), 17);

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate.clone()]))
                .with_pull_delta_proposal(Ok(candidate))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Err("Verification failed".to_string()));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(()));
        let metadata = Arc::new(mock_metadata);

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(mock_network),
            metadata.clone(),
        );

        // retry_count 17 + 1 = 18 >= max_retries 18; the default config
        // retains (TTL > 0).
        let config = CanonicalizationConfig::new(10, 18);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        assert!(storage.get_delete_delta_calls().is_empty());
        let status_writes = storage.get_update_delta_status_calls();
        assert_eq!(status_writes.len(), 1);
        assert!(status_writes[0].2.is_retained());
        assert_eq!(
            status_writes[0].2.retain_reason(),
            Some(crate::delta_object::RetainReason::RetryExhausted)
        );
        assert_eq!(
            storage.delete_delta_proposal_calls.lock().unwrap().len(),
            1,
            "the matching proposal is deleted at retain time"
        );
        assert!(
            metadata
                .get_set_calls()
                .iter()
                .any(|m| !m.has_pending_candidate),
            "retention releases the account lock"
        );
    }

    #[tokio::test]
    async fn test_max_retries_with_abandon_intent_finalizes_abandon() {
        // Retry exhaustion with a recorded abandon intent resolves the
        // abandon as Discarded{client_abandoned} — queryable history,
        // symmetric with the divergence path — instead of the historical
        // delete that left the client's status poll on `not found`. Only
        // RPC-failure exhaustion reaches here (at-base reads route to
        // the abandon-confirmation path), and the reconcile net still
        // promotes the delta if its transaction actually landed.
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status = DeltaStatus::Candidate {
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            retry_count: 17,
            divergence_count: 0,
            abandon_requested_at: Some("2024-01-01T00:00:01Z".to_string()),
            abandon_confirm_count: 0,
        };

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Err("Verification failed".to_string()));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(()));

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::new(10, 18);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        assert!(storage.get_delete_delta_calls().is_empty());
        let status_writes = storage.get_update_delta_status_calls();
        assert_eq!(status_writes.len(), 1);
        assert!(
            status_writes[0].2.is_client_abandoned(),
            "the abandon resolves as history, not a delete; got {:?}",
            status_writes[0].2
        );
    }

    #[tokio::test]
    async fn test_max_retries_retention_disabled_discards() {
        // `retained_ttl_seconds = 0` restores the historical behavior:
        // retry exhaustion deletes the candidate.
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status =
            DeltaStatus::candidate_with_retry("2024-01-01T00:00:00Z".to_string(), 17);

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Err("Verification failed".to_string()));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(()));

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::new(10, 18).with_retained_ttl_seconds(0);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        assert_eq!(
            storage.get_delete_delta_calls(),
            vec![(account_id.to_string(), 1)]
        );
        assert!(storage.get_update_delta_status_calls().is_empty());
    }

    #[tokio::test]
    async fn test_spuriously_diverged_retained_delta_heals_once_chain_shows_it() {
        // End-to-end replay of the #312 incident class (issue #345): a
        // candidate wrongly condemned as diverged — e.g. the pre-#326
        // empty-digest misread of a new account's first transaction, or a
        // lagging RPC node — is parked as retained(diverged). Once the
        // chain shows the delta's commitment, the reconcile pass promotes
        // it and the account recovers without any client involvement.
        let account_id = "0xtest_account";
        let mut retained = create_retained_delta(account_id, 1, "2024-01-01T00:00:00Z");
        retained.status = DeltaStatus::retained(
            "2024-01-01T00:00:00Z".to_string(),
            crate::delta_object::RetainReason::Diverged,
        );

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_list_accounts_with_recoverable_deltas(Ok(vec![account_id.to_string()]))
                .with_pull_recoverable_deltas(Ok(vec![retained]))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_submit_state(Ok(()))
                .with_submit_delta(Ok(())),
        );

        let mock_network = Arc::new(
            MockNetworkClient::new()
                .with_apply_delta(Ok((
                    serde_json::json!({"new": "state"}),
                    "new_commitment".to_string(),
                )))
                // The transaction has landed by now: the probe of the
                // stored base observes the on-chain commitment the
                // diverged verdict said could never appear.
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: "new_commitment".to_string(),
                }))
                .with_should_update_auth(Ok(None)),
        );

        let mut account_metadata = create_test_metadata(account_id);
        account_metadata.has_pending_candidate = false;
        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![]))
            .with_get(Ok(Some(account_metadata)))
            .with_set(Ok(()));

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 10, 0).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            mock_network.clone(),
            Arc::new(mock_metadata),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18);
        let processor = reconcile_processor(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        let states = storage.get_submit_state_calls();
        assert_eq!(states.len(), 1, "the stuck base catches up to the chain");
        assert_eq!(states[0].commitment, "new_commitment");
        assert!(
            storage
                .get_submit_delta_calls()
                .iter()
                .any(|d| d.status.is_canonical()),
            "the wrongly-condemned delta ends canonical, not deleted"
        );
        assert!(storage.get_delete_delta_calls().is_empty());
    }

    #[tokio::test]
    async fn test_superseded_retained_row_is_never_promoted() {
        // Between the reconcile read and the promotion write, a client
        // submission can supersede the retained row with a brand-new
        // candidate at the same nonce. The promotion's source-kind gate
        // must then make the write a no-op — a union gate would stamp
        // the client's fresh candidate canonical with the old delta's
        // commitment.
        let account_id = "0xtest_account";
        let retained = create_retained_delta(account_id, 1, "2024-01-01T00:00:00Z");
        // What the store holds by promotion time: a fresh candidate.
        let superseding = create_candidate_delta(account_id, 1);

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_list_accounts_with_recoverable_deltas(Ok(vec![account_id.to_string()]))
                .with_pull_recoverable_deltas(Ok(vec![retained]))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                // The sequential promotion re-reads the row for the
                // source-kind gate and finds the superseding candidate.
                .with_pull_delta(Ok(superseding)),
        );

        let mock_network = Arc::new(
            MockNetworkClient::new()
                .with_apply_delta(Ok((
                    serde_json::json!({"new": "state"}),
                    "new_commitment".to_string(),
                )))
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: "new_commitment".to_string(),
                }))
                .with_should_update_auth(Ok(None)),
        );

        let mut account_metadata = create_test_metadata(account_id);
        account_metadata.has_pending_candidate = false;
        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![]))
            .with_get(Ok(Some(account_metadata)))
            .with_set(Ok(()));

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 10, 0).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            mock_network.clone(),
            Arc::new(mock_metadata),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18);
        let processor = reconcile_processor(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        assert!(
            storage.get_submit_state_calls().is_empty(),
            "a superseded promotion must not advance the state"
        );
        assert!(
            storage.get_submit_delta_calls().is_empty(),
            "the superseding candidate must not be stamped canonical"
        );
    }

    #[tokio::test]
    async fn test_late_landing_abandoned_delta_reconciles() {
        // The issue #319 synergy: the abandon quarantine cannot fully
        // rule out a late-landing transaction, and one that lands after
        // the abandon finalizes leaves stored state behind chain —
        // exactly the #345 scenario, with the preserved client-abandoned
        // discard holding everything needed to recover. The reconcile
        // sweep picks it up and promotes it once the chain shows it.
        let account_id = "0xtest_account";
        let mut abandoned = create_candidate_delta(account_id, 1);
        abandoned.status =
            DeltaStatus::discarded_client_abandoned("2024-01-01T00:00:00Z".to_string());

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_list_accounts_with_recoverable_deltas(Ok(vec![account_id.to_string()]))
                .with_pull_recoverable_deltas(Ok(vec![abandoned]))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_submit_state(Ok(()))
                .with_submit_delta(Ok(())),
        );

        let mock_network = Arc::new(
            MockNetworkClient::new()
                .with_apply_delta(Ok((
                    serde_json::json!({"new": "state"}),
                    "new_commitment".to_string(),
                )))
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: "new_commitment".to_string(),
                }))
                .with_should_update_auth(Ok(None)),
        );

        let mut account_metadata = create_test_metadata(account_id);
        account_metadata.has_pending_candidate = false;
        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![]))
            .with_get(Ok(Some(account_metadata)))
            .with_set(Ok(()));

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 10, 0).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            mock_network.clone(),
            Arc::new(mock_metadata),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18);
        let processor = reconcile_processor(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        let states = storage.get_submit_state_calls();
        assert_eq!(states.len(), 1, "the late-landed state is recovered");
        assert_eq!(states[0].commitment, "new_commitment");
        assert!(
            storage
                .get_submit_delta_calls()
                .iter()
                .any(|d| d.status.is_canonical()),
            "the abandoned delta ends canonical"
        );
    }

    #[tokio::test]
    async fn test_expired_abandoned_delta_is_skipped_not_deleted() {
        // Unlike an expired retained row, a client-abandoned discard past
        // the TTL is history by design (issue #319) and must never be
        // deleted by the reconcile sweep.
        let account_id = "0xtest_account";
        let mut abandoned = create_candidate_delta(account_id, 1);
        abandoned.status =
            DeltaStatus::discarded_client_abandoned("2024-01-01T00:00:00Z".to_string());

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_list_accounts_with_recoverable_deltas(Ok(vec![account_id.to_string()]))
                .with_pull_recoverable_deltas(Ok(vec![abandoned])),
        );

        let mock_network = Arc::new(MockNetworkClient::new());

        let mut account_metadata = create_test_metadata(account_id);
        account_metadata.has_pending_candidate = false;
        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![]))
            .with_get(Ok(Some(account_metadata)))
            .with_set(Ok(()));

        // Two days later: past the one-day default TTL.
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 3, 0, 0, 0).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            mock_network.clone(),
            Arc::new(mock_metadata),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18);
        let processor = reconcile_processor(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        assert!(storage.get_discard_candidate_calls().is_empty());
        assert!(storage.get_delete_delta_calls().is_empty());
        assert!(storage.get_submit_state_calls().is_empty());
    }

    #[tokio::test]
    async fn test_reconcile_promotes_matching_retained_delta() {
        // The core issue #345 recovery: a retained delta whose recomputed
        // commitment now matches the chain is promoted to canonical, and
        // the stored base catches up to the on-chain nonce. The account
        // enters the pass through the retained-account scan, not the
        // pending-candidate flag.
        let account_id = "0xtest_account";
        let retained = create_retained_delta(account_id, 1, "2024-01-01T00:00:00Z");

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_list_accounts_with_recoverable_deltas(Ok(vec![account_id.to_string()]))
                .with_pull_recoverable_deltas(Ok(vec![retained]))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_submit_state(Ok(()))
                .with_submit_delta(Ok(())),
        );

        let mock_network = Arc::new(
            MockNetworkClient::new()
                .with_apply_delta(Ok((
                    serde_json::json!({"new": "state"}),
                    "new_commitment".to_string(),
                )))
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: "new_commitment".to_string(),
                }))
                .with_should_update_auth(Ok(None)),
        );

        let mut account_metadata = create_test_metadata(account_id);
        account_metadata.has_pending_candidate = false;
        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![]))
            .with_get(Ok(Some(account_metadata)))
            .with_set(Ok(()));

        // Ten minutes into retention: well inside the default TTL.
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 10, 0).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            mock_network.clone(),
            Arc::new(mock_metadata),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18);
        let processor = reconcile_processor(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        let states = storage.get_submit_state_calls();
        assert_eq!(states.len(), 1, "reconcile advances the stored state");
        assert_eq!(states[0].commitment, "new_commitment");
        let deltas = storage.get_submit_delta_calls();
        assert!(
            deltas.iter().any(|d| d.status.is_canonical()),
            "the retained delta is promoted to canonical"
        );
        assert!(storage.get_delete_delta_calls().is_empty());
    }

    #[tokio::test]
    async fn test_reconcile_defers_while_chain_lags() {
        // No match yet: the retained delta is left untouched for the next
        // tick — no retry budget applies, the TTL is the only bound.
        let account_id = "0xtest_account";
        let retained = create_retained_delta(account_id, 1, "2024-01-01T00:00:00Z");

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_list_accounts_with_recoverable_deltas(Ok(vec![account_id.to_string()]))
                .with_pull_recoverable_deltas(Ok(vec![retained]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = Arc::new(
            MockNetworkClient::new()
                .with_apply_delta(Ok((
                    serde_json::json!({"new": "state"}),
                    "new_commitment".to_string(),
                )))
                // The chain still sits at the stored base: the account
                // probe answers Match and the pass stops right there.
                .with_verify_commitment(Ok(StateVerification::Match)),
        );

        let mut account_metadata = create_test_metadata(account_id);
        account_metadata.has_pending_candidate = false;
        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![]))
            .with_get(Ok(Some(account_metadata)))
            .with_set(Ok(()));

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 10, 0).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            mock_network.clone(),
            Arc::new(mock_metadata),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18);
        let processor = reconcile_processor(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        assert!(storage.get_submit_state_calls().is_empty());
        assert!(storage.get_delete_delta_calls().is_empty());
        assert!(storage.get_update_delta_status_calls().is_empty());
        // The no-advance probe is the whole cost: one chain RPC against
        // the stored base, and no reconstruction at all.
        assert_eq!(
            mock_network.get_verify_commitment_calls(),
            vec![(account_id.to_string(), "prev_commitment".to_string())],
        );
        assert_eq!(
            mock_network.apply_delta_responses.lock().unwrap().len(),
            1,
            "the queued apply_delta response was never consumed"
        );
    }

    #[tokio::test]
    async fn test_reconcile_expires_retained_delta_past_ttl() {
        // A retained delta that outlives the TTL without ever verifying
        // is dropped for good, together with its proposal path.
        let account_id = "0xtest_account";
        let retained = create_retained_delta(account_id, 1, "2024-01-01T00:00:00Z");

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_list_accounts_with_recoverable_deltas(Ok(vec![account_id.to_string()]))
                .with_pull_recoverable_deltas(Ok(vec![retained.clone()]))
                // The sequential discard re-reads the row to enforce the
                // expected-kind guard.
                .with_pull_delta(Ok(retained)),
        );

        let mock_network = Arc::new(MockNetworkClient::new());

        let mut account_metadata = create_test_metadata(account_id);
        account_metadata.has_pending_candidate = false;
        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![]))
            .with_get(Ok(Some(account_metadata)))
            .with_set(Ok(()));

        // Two days after retention began: past the one-day default TTL.
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 3, 0, 0, 0).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            mock_network.clone(),
            Arc::new(mock_metadata),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18);
        let processor = reconcile_processor(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        assert_eq!(
            storage.get_discard_candidate_calls(),
            vec![(
                account_id.to_string(),
                1,
                crate::storage::DeltaStatusKind::Retained
            )]
        );
        assert_eq!(
            storage.get_delete_delta_calls(),
            vec![(account_id.to_string(), 1)]
        );
        assert!(storage.get_submit_state_calls().is_empty());
    }

    #[tokio::test]
    async fn test_reconcile_waits_for_in_flight_candidate() {
        // While a candidate is pending, retained deltas are not touched:
        // a reconcile promotion would move the stored base out from under
        // the in-flight (signed) candidate — the exact race issue #345
        // rejects for a syncState-driven write.
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 2);
        let retained = create_retained_delta(account_id, 1, "2024-01-01T00:00:00Z");

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_list_accounts_with_recoverable_deltas(Ok(vec![account_id.to_string()]))
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_recoverable_deltas(Ok(vec![retained]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );

        let mock_network = Arc::new(MockNetworkClient::new().with_apply_delta(Ok((
            serde_json::json!({"new": "state"}),
            "new_commitment".to_string(),
        ))));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(()));

        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            mock_network.clone(),
            Arc::new(mock_metadata),
            clock,
        );

        let config = CanonicalizationConfig::new(10, 18).with_submission_grace_period_seconds(600);
        let processor = reconcile_processor(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        assert_eq!(
            storage
                .pull_recoverable_deltas_responses
                .lock()
                .unwrap()
                .len(),
            1,
            "the retained queue is never read while a candidate is in flight"
        );
        assert!(
            mock_network.get_verify_commitment_calls().is_empty(),
            "no chain probe while a candidate is in flight"
        );
        assert!(storage.get_submit_state_calls().is_empty());
    }

    #[tokio::test]
    async fn test_candidate_pass_stops_at_first_unresolved_nonce() {
        // When candidate N defers, candidate N+1 still chains from a
        // state the store has not reached: processing it would fail
        // reconstruction or emit false commitment-mismatch signals. The
        // pass must stop at the break in the chain.
        let account_id = "0xtest_account";
        let first = create_candidate_delta(account_id, 1);
        let mut second = create_candidate_delta(account_id, 2);
        second.prev_commitment = "new_commitment".to_string();

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![first, second]))
                // First candidate's verification read, then the loop's
                // chain check before the second candidate.
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id))),
        );
        let mock_network = Arc::new(
            MockNetworkClient::new()
                .with_apply_delta(Ok((
                    serde_json::json!({"new": "state"}),
                    "new_commitment".to_string(),
                )))
                .with_apply_delta(Ok((
                    serde_json::json!({"newer": "state"}),
                    "newer_commitment".to_string(),
                )))
                // Chain still at the base: candidate 1 defers in grace.
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: "prev_commitment".to_string(),
                })),
        );
        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))));
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 5).unwrap(),
        ));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            mock_network.clone(),
            Arc::new(mock_metadata),
            clock,
        );
        let processor = DeltasProcessor::new(
            state,
            CanonicalizationConfig::new(10, 18).with_submission_grace_period_seconds(600),
        );

        processor.process_all_accounts().await.expect("pass runs");

        assert_eq!(
            mock_network.get_verify_commitment_calls().len(),
            1,
            "the successor nonce is never verified after its predecessor deferred"
        );
        assert_eq!(
            mock_network.apply_delta_responses.lock().unwrap().len(),
            1,
            "the successor nonce is never reconstructed"
        );
    }

    #[tokio::test]
    async fn test_reconcile_backoff_skips_account_until_due() {
        // Worker passes back off aged recoverable rows: an account whose
        // youngest row's age falls between grid points is skipped without
        // a chain probe (or any network work); at a grid point it probes.
        let account_id = "0xtest_account";

        let run_at = |now: chrono::DateTime<Utc>| {
            let retained = create_retained_delta(account_id, 1, "2024-01-01T00:00:00Z");
            let storage = Arc::new(
                MockStorageBackend::new()
                    .with_list_accounts_with_recoverable_deltas(Ok(vec![account_id.to_string()]))
                    .with_pull_recoverable_deltas(Ok(vec![retained]))
                    .with_pull_state(Ok(create_test_state(account_id))),
            );
            let mock_network = Arc::new(
                MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Match)),
            );
            let state = create_test_app_state_with_clock(
                storage.clone(),
                mock_network.clone(),
                Arc::new(MockMetadataStore::new()),
                Arc::new(MockClock::new(now)),
            );
            let pass = PassLease::single_process();
            let processor = DeltasProcessor::with_controls(
                state,
                CanonicalizationConfig::new(10, 18),
                pass.leader,
                pass.lease,
                pass.cancel,
                ProcessingMode::ReconcileRecoverable,
                PassControls {
                    fast_state: Arc::new(FastPromotionState::default()),
                    reconcile_state: Arc::new(ReconcileState::default()),
                    deadline: None,
                    reconcile_backoff: true,
                },
            );
            (processor, mock_network)
        };

        // Age 3720s: past the warm period, spacing capped at 600s, and
        // 3720 % 600 = 120 >= the 60s interval — not due.
        let (processor, network) = run_at(Utc.with_ymd_and_hms(2024, 1, 1, 1, 2, 0).unwrap());
        processor.process_all_accounts().await.expect("pass runs");
        assert!(
            network.get_verify_commitment_calls().is_empty(),
            "a backed-off account is skipped without a chain probe"
        );

        // Age 3617s: 3617 % 600 = 17 < 60 — due, so the probe runs.
        let (processor, network) = run_at(Utc.with_ymd_and_hms(2024, 1, 1, 1, 0, 17).unwrap());
        processor.process_all_accounts().await.expect("pass runs");
        assert_eq!(network.get_verify_commitment_calls().len(), 1);
    }

    #[tokio::test]
    async fn test_reconcile_page_rotates_across_passes() {
        // A backlog larger than one page drains across passes: the
        // rotation cursor starts each pass after the previous page's
        // last account, wrapping around, so every account is eventually
        // probed.
        let accounts = ["0xaccount_a", "0xaccount_b", "0xaccount_c"];
        let mut storage = MockStorageBackend::new();
        for _ in 0..2 {
            storage = storage.with_list_accounts_with_recoverable_deltas(Ok(accounts
                .iter()
                .map(|a| a.to_string())
                .collect()));
        }
        for _ in 0..4 {
            storage = storage
                .with_pull_recoverable_deltas(Ok(vec![create_retained_delta(
                    "0xaccount_a",
                    1,
                    "2024-01-01T00:00:00Z",
                )]))
                .with_pull_state(Ok(create_test_state("0xaccount_a")));
        }
        let storage = Arc::new(storage);
        let mut network = MockNetworkClient::new();
        for _ in 0..4 {
            network = network.with_verify_commitment(Ok(StateVerification::Match));
        }
        let mock_network = Arc::new(network);
        let state = create_test_app_state_with_clock(
            storage.clone(),
            mock_network.clone(),
            Arc::new(MockMetadataStore::new()),
            Arc::new(MockClock::new(
                Utc.with_ymd_and_hms(2024, 1, 1, 0, 10, 0).unwrap(),
            )),
        );
        let pass = PassLease::single_process();
        let processor = DeltasProcessor::with_controls(
            state,
            CanonicalizationConfig::new(10, 18).with_reconcile_page_size(2),
            pass.leader,
            pass.lease,
            pass.cancel,
            ProcessingMode::ReconcileRecoverable,
            PassControls {
                fast_state: Arc::new(FastPromotionState::default()),
                reconcile_state: Arc::new(ReconcileState::default()),
                deadline: None,
                reconcile_backoff: false,
            },
        );

        processor.process_all_accounts().await.expect("first pass");
        let first_page: Vec<String> = mock_network
            .get_verify_commitment_calls()
            .into_iter()
            .map(|(account_id, _)| account_id)
            .collect();
        assert_eq!(first_page.len(), 2, "one page per pass");

        processor.process_all_accounts().await.expect("second pass");
        let probed: std::collections::HashSet<String> = mock_network
            .get_verify_commitment_calls()
            .into_iter()
            .map(|(account_id, _)| account_id)
            .collect();
        assert_eq!(
            probed.len(),
            accounts.len(),
            "rotation covers every account within two passes"
        );
    }

    #[tokio::test]
    async fn test_reconcile_retries_stranded_proposal_cleanup() {
        // A proposal whose deletion failed at retain time is retried on
        // every due reconcile visit, even when the chain has not moved.
        let account_id = "0xtest_account";
        let retained = create_retained_delta(account_id, 1, "2024-01-01T00:00:00Z");

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_list_accounts_with_recoverable_deltas(Ok(vec![account_id.to_string()]))
                .with_pull_recoverable_deltas(Ok(vec![retained.clone()]))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_delta_proposal(Ok(retained))
                .with_delete_delta_proposal(Ok(())),
        );
        let mock_network =
            Arc::new(MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Match)));
        let state = create_test_app_state_with_clock(
            storage.clone(),
            mock_network.clone(),
            Arc::new(MockMetadataStore::new()),
            Arc::new(MockClock::new(
                Utc.with_ymd_and_hms(2024, 1, 1, 0, 10, 0).unwrap(),
            )),
        );
        let processor = reconcile_processor(state, CanonicalizationConfig::new(10, 18));

        processor.process_all_accounts().await.expect("pass runs");

        assert_eq!(
            storage.get_delete_delta_proposal_calls().len(),
            1,
            "the stranded proposal is cleaned up during the reconcile visit"
        );
    }

    #[tokio::test]
    async fn test_reconcile_skips_reconstruction_when_hint_disagrees() {
        // The chain advanced, but the retained row's submission-computed
        // end state is not what the chain shows: the hint rules the row
        // out without reconstructing anything.
        let account_id = "0xtest_account";
        let mut retained = create_retained_delta(account_id, 1, "2024-01-01T00:00:00Z");
        retained.new_commitment = Some("some_other_commitment".to_string());

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_list_accounts_with_recoverable_deltas(Ok(vec![account_id.to_string()]))
                .with_pull_recoverable_deltas(Ok(vec![retained]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );
        let mock_network = Arc::new(
            MockNetworkClient::new()
                .with_apply_delta(Ok((
                    serde_json::json!({"new": "state"}),
                    "some_other_commitment".to_string(),
                )))
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: "new_commitment".to_string(),
                })),
        );
        let state = create_test_app_state_with_clock(
            storage.clone(),
            mock_network.clone(),
            Arc::new(MockMetadataStore::new()),
            Arc::new(MockClock::new(
                Utc.with_ymd_and_hms(2024, 1, 1, 0, 10, 0).unwrap(),
            )),
        );
        let processor = reconcile_processor(state, CanonicalizationConfig::new(10, 18));

        processor.process_all_accounts().await.expect("pass runs");

        assert!(storage.get_submit_state_calls().is_empty());
        assert_eq!(
            mock_network.apply_delta_responses.lock().unwrap().len(),
            1,
            "the hint mismatch skipped reconstruction entirely"
        );
    }

    #[tokio::test]
    async fn test_reconcile_never_promotes_obsolete_retained_row() {
        // A retained row whose base is no longer the stored canonical
        // commitment (e.g. the head moved via configure after the row
        // was retained) is structurally obsolete: it can never promote —
        // the base gate rules it out before reconstruction, and the TTL
        // terminalizes it. UNIQUE(account_id, nonce) plus the atomic
        // prev-commitment admission gate mean this is the only way an
        // out-of-date recoverable row can exist at all.
        let account_id = "0xtest_account";
        let mut obsolete = create_retained_delta(account_id, 1, "2024-01-01T00:00:00Z");
        obsolete.prev_commitment = "stale_base_commitment".to_string();

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_list_accounts_with_recoverable_deltas(Ok(vec![account_id.to_string()]))
                .with_pull_recoverable_deltas(Ok(vec![obsolete]))
                .with_pull_state(Ok(create_test_state(account_id))),
        );
        let mock_network = Arc::new(
            MockNetworkClient::new()
                .with_apply_delta(Ok((
                    serde_json::json!({"new": "state"}),
                    "new_commitment".to_string(),
                )))
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: "new_commitment".to_string(),
                })),
        );
        let state = create_test_app_state_with_clock(
            storage.clone(),
            mock_network.clone(),
            Arc::new(MockMetadataStore::new()),
            Arc::new(MockClock::new(
                Utc.with_ymd_and_hms(2024, 1, 1, 0, 10, 0).unwrap(),
            )),
        );
        let processor = reconcile_processor(state, CanonicalizationConfig::new(10, 18));

        processor.process_all_accounts().await.expect("pass runs");

        assert!(
            storage.get_submit_state_calls().is_empty(),
            "an obsolete row must never advance the state"
        );
        assert_eq!(
            mock_network.apply_delta_responses.lock().unwrap().len(),
            1,
            "the base gate skipped reconstruction entirely"
        );
    }

    #[tokio::test]
    async fn test_missing_claimed_commitment_promotes_with_recomputed() {
        // A verified candidate without a client-claimed commitment must not
        // stay a candidate forever: verification proved the recomputed
        // commitment, so promotion proceeds with it.
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.new_commitment = None;

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate.clone()]))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_submit_state(Ok(()))
                .with_submit_delta(Ok(())),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "recomputed_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Match))
            .with_should_update_auth(Ok(None));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(()));

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        let submitted = storage.get_submit_state_calls();
        assert_eq!(submitted.len(), 1);
        assert_eq!(submitted[0].commitment, "recomputed_commitment");
        let deltas = storage.get_submit_delta_calls();
        assert_eq!(deltas.len(), 1);
        assert_eq!(
            deltas[0].new_commitment.as_deref(),
            Some("recomputed_commitment")
        );
    }

    #[tokio::test]
    async fn test_claim_mismatch_promotes_with_recomputed_commitment() {
        // The chain matched the recomputed commitment, so a differing
        // client claim never blocks promotion — the verified value is
        // what gets persisted.
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.new_commitment = Some("claimed_commitment".to_string());

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate.clone()]))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_submit_state(Ok(()))
                .with_submit_delta(Ok(())),
        );

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "recomputed_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Match))
            .with_should_update_auth(Ok(None));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(()));

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());

        let submitted = storage.get_submit_state_calls();
        assert_eq!(submitted.len(), 1);
        assert_eq!(submitted[0].commitment, "recomputed_commitment");
        let deltas = storage.get_submit_delta_calls();
        assert_eq!(deltas.len(), 1);
        assert_eq!(
            deltas[0].new_commitment.as_deref(),
            Some("recomputed_commitment")
        );
    }

    #[tokio::test]
    async fn test_process_candidate_apply_delta_fails() {
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);

        let mock_storage = MockStorageBackend::new()
            .with_pull_deltas_after(Ok(vec![candidate.clone()]))
            .with_pull_state(Ok(create_test_state(account_id)));

        let mock_network =
            MockNetworkClient::new().with_apply_delta(Err("Apply delta failed".to_string()));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let summary = processor
            .process_all_accounts()
            .await
            .expect("candidate failures do not abort the pass");
        assert_eq!(summary.failed_accounts, 1);
    }

    #[tokio::test]
    async fn test_canonicalize_with_auth_update() {
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);

        let new_auth = Auth::MidenFalconRpo {
            cosigner_commitments: vec!["0xnew_commitment".to_string()],
        };

        let mock_storage = MockStorageBackend::new()
            .with_pull_deltas_after(Ok(vec![candidate.clone()]))
            .with_pull_state(Ok(create_test_state(account_id)))
            .with_pull_state(Ok(create_test_state(account_id)))
            .with_pull_state(Ok(create_test_state(account_id)))
            .with_submit_state(Ok(()))
            .with_submit_delta(Ok(()));

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Match))
            .with_should_update_auth(Ok(Some(new_auth)));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(())) // For update_auth
            .with_set(Ok(())); // For clearing has_pending_candidate

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_deltas_processor_new() {
        let mock_storage = MockStorageBackend::new();
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new();

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::new(5, 30);
        let _processor = DeltasProcessor::new(state, config);
        // Just verify it constructs without panic
    }

    #[tokio::test]
    async fn test_test_deltas_processor_new() {
        let mock_storage = MockStorageBackend::new();
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new();

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let _processor = TestDeltasProcessor::new(state);
        // Just verify it constructs without panic
    }

    #[tokio::test]
    async fn test_process_multiple_accounts() {
        let account_id_1 = "0xtest_account_1";
        let account_id_2 = "0xtest_account_2";

        let mock_storage = MockStorageBackend::new()
            .with_pull_deltas_after(Ok(vec![])) // First account has no deltas
            .with_pull_deltas_after(Ok(vec![])); // Second account has no deltas

        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![
                account_id_1.to_string(),
                account_id_2.to_string(),
            ]))
            .with_get(Ok(Some(create_test_metadata(account_id_1))))
            .with_get(Ok(Some(create_test_metadata(account_id_2))));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_process_account_directly() {
        let account_id = "0xtest_account";

        let mock_storage = MockStorageBackend::new().with_pull_deltas_after(Ok(vec![]));
        let mock_network = MockNetworkClient::new();
        let mock_metadata =
            MockMetadataStore::new().with_get(Ok(Some(create_test_metadata(account_id))));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_account(account_id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_test_processor_process_account() {
        let account_id = "0xtest_account";

        let mock_storage = MockStorageBackend::new().with_pull_deltas_after(Ok(vec![]));
        let mock_network = MockNetworkClient::new();
        let mock_metadata =
            MockMetadataStore::new().with_get(Ok(Some(create_test_metadata(account_id))));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let processor = TestDeltasProcessor::new(state);

        let result = processor.process_account(account_id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_canonicalize_with_existing_proposal() {
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);

        let mock_storage = MockStorageBackend::new()
            .with_pull_deltas_after(Ok(vec![candidate.clone()]))
            .with_pull_state(Ok(create_test_state(account_id)))
            .with_pull_state(Ok(create_test_state(account_id)))
            .with_submit_state(Ok(()))
            .with_submit_delta(Ok(()))
            .with_pull_delta_proposal(Ok(candidate.clone())) // Proposal exists
            .with_delete_delta_proposal(Ok(()));

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Match))
            .with_should_update_auth(Ok(None));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(())); // For clearing has_pending_candidate

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_canonicalize_delete_proposal_fails() {
        let account_id = "0xtest_account";
        let candidate = create_candidate_delta(account_id, 1);

        let mock_storage = MockStorageBackend::new()
            .with_pull_deltas_after(Ok(vec![candidate.clone()]))
            .with_pull_state(Ok(create_test_state(account_id)))
            .with_pull_state(Ok(create_test_state(account_id)))
            .with_submit_state(Ok(()))
            .with_submit_delta(Ok(()))
            .with_pull_delta_proposal(Ok(candidate.clone()))
            .with_delete_delta_proposal(Err("Delete failed".to_string())); // Delete fails

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Match))
            .with_should_update_auth(Ok(None));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(())); // For clearing has_pending_candidate

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        // Should succeed even if proposal delete fails (just logs warning)
        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_test_processor_process_all_accounts() {
        let mock_storage = MockStorageBackend::new();
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new().with_list_with_pending_candidates(Ok(vec![]));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let processor = TestDeltasProcessor::new(state);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
    }

    /// Elector standing in for the Postgres-backed one: leases carry a
    /// shared-store row, so the processor must attach a fence to every
    /// canonicalization write.
    struct FencingElector;

    #[async_trait::async_trait]
    impl crate::coordination::LeaderElector for FencingElector {
        async fn try_acquire(&self, _ttl: std::time::Duration) -> Result<Option<Lease>> {
            Ok(None)
        }

        async fn renew(&self, _lease: &Lease, _ttl: std::time::Duration) -> Result<bool> {
            Ok(true)
        }

        async fn verify_held(&self, _lease: &Lease) -> Result<bool> {
            Ok(true)
        }

        async fn release(&self, _lease: Lease) -> Result<()> {
            Ok(())
        }

        fn supports_fencing(&self) -> bool {
            true
        }
    }

    fn fenced_processor(state: AppState, config: CanonicalizationConfig) -> DeltasProcessor {
        DeltasProcessor::with_lease_and_mode(
            state,
            config,
            Arc::new(FencingElector),
            Lease {
                name: CANONICALIZATION_LEASE.to_string(),
                holder_id: "replica-a".to_string(),
                fence_token: 7,
                expires_at: chrono::DateTime::<Utc>::MAX_UTC,
            },
            tokio_util::sync::CancellationToken::new(),
            ProcessingMode::Full,
        )
    }

    #[tokio::test]
    async fn concurrent_pass_processes_every_account() {
        // Three accounts under the default concurrency of 4: every account
        // promotes its candidate, and the summary reflects a clean pass.
        // All mock responses are identical, so completion order is free.
        let account_ids: Vec<String> = (1..=3).map(|i| format!("0xtest_account_{i}")).collect();

        let mut storage = MockStorageBackend::new();
        let mut network = MockNetworkClient::new();
        let mut metadata =
            MockMetadataStore::new().with_list_with_pending_candidates(Ok(account_ids.clone()));
        for account_id in &account_ids {
            storage = storage
                .with_pull_candidate_deltas(Ok(vec![create_candidate_delta(account_id, 1)]))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_submit_state(Ok(()))
                .with_submit_delta(Ok(()));
            network = network
                .with_apply_delta(Ok((
                    serde_json::json!({"new": "state"}),
                    "new_commitment".to_string(),
                )))
                .with_verify_commitment(Ok(StateVerification::Match))
                .with_should_update_auth(Ok(None));
            metadata = metadata
                .with_get(Ok(Some(create_test_metadata(account_id))))
                .with_get(Ok(Some(create_test_metadata(account_id))))
                .with_set(Ok(()));
        }
        let storage = Arc::new(storage);

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(network),
            Arc::new(metadata),
        );

        let processor = DeltasProcessor::new(state, CanonicalizationConfig::default());

        let summary = processor
            .process_all_accounts()
            .await
            .expect("pass completes");

        assert_eq!(summary.accounts, 3);
        assert_eq!(summary.failed_accounts, 0);
        assert!(!summary.cancelled);
        assert_eq!(
            storage.get_submit_state_calls().len(),
            3,
            "every account's candidate must promote",
        );
    }

    #[tokio::test]
    async fn cancelled_pass_admits_no_account_work() {
        // A pre-cancelled token: no account task starts. The mock queues
        // are deliberately empty — had any account been processed, its
        // metadata read would have failed and the summary would count it.
        let account_ids = vec![
            "0xtest_account_1".to_string(),
            "0xtest_account_2".to_string(),
        ];

        let storage = Arc::new(MockStorageBackend::new());
        let metadata = MockMetadataStore::new().with_list_with_pending_candidates(Ok(account_ids));

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(MockNetworkClient::new()),
            Arc::new(metadata),
        );

        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let pass = PassLease::single_process();
        let processor = DeltasProcessor::with_lease_and_mode(
            state,
            CanonicalizationConfig::default(),
            pass.leader,
            pass.lease,
            cancel,
            ProcessingMode::Full,
        );

        let summary = processor
            .process_all_accounts()
            .await
            .expect("cancelled pass still reports a summary");

        assert_eq!(summary.accounts, 2);
        assert_eq!(summary.failed_accounts, 0);
        assert!(summary.cancelled);
        assert!(storage.get_submit_state_calls().is_empty());
    }

    fn promotion_mocks(
        account_id: &str,
    ) -> (MockStorageBackend, MockNetworkClient, MockMetadataStore) {
        let candidate = create_candidate_delta(account_id, 1);
        let storage = MockStorageBackend::new()
            .with_pull_deltas_after(Ok(vec![candidate.clone()]))
            .with_pull_state(Ok(create_test_state(account_id)))
            .with_pull_state(Ok(create_test_state(account_id)))
            .with_pull_state(Ok(create_test_state(account_id)))
            .with_submit_state(Ok(()))
            .with_submit_delta(Ok(()));
        let network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Ok(StateVerification::Match))
            .with_should_update_auth(Ok(None));
        let metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(()));
        (storage, network, metadata)
    }

    #[tokio::test]
    async fn promotion_carries_the_lease_fence_to_storage() {
        let account_id = "0xtest_account";
        let (storage, network, metadata) = promotion_mocks(account_id);
        let storage = Arc::new(storage);

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(network),
            Arc::new(metadata),
        );
        let processor = fenced_processor(state, CanonicalizationConfig::default());

        processor
            .process_all_accounts()
            .await
            .expect("pass completes");
        assert_eq!(
            storage.get_promote_candidate_fences(),
            vec![Some(crate::storage::LeaseFence {
                lease_name: CANONICALIZATION_LEASE.to_string(),
                holder_id: "replica-a".to_string(),
                fence_token: 7,
            })],
            "a fencing elector's lease identity must reach the storage write",
        );
    }

    #[tokio::test]
    async fn single_process_promotion_carries_no_fence() {
        let account_id = "0xtest_account";
        let (storage, network, metadata) = promotion_mocks(account_id);
        let storage = Arc::new(storage);

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(network),
            Arc::new(metadata),
        );
        let processor = DeltasProcessor::new(state, CanonicalizationConfig::default());

        processor
            .process_all_accounts()
            .await
            .expect("pass completes");
        assert_eq!(
            storage.get_promote_candidate_fences(),
            vec![None],
            "single-process leases have no shared-store row to fence on",
        );
    }

    #[tokio::test]
    async fn stale_lease_promotion_skips_proposal_cleanup() {
        let account_id = "0xtest_account";
        let (storage, network, metadata) = promotion_mocks(account_id);
        let storage =
            Arc::new(storage.with_promote_candidate(Ok(crate::storage::PromoteWrite::StaleLease)));

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(network),
            Arc::new(metadata),
        );
        let processor = fenced_processor(state, CanonicalizationConfig::default());

        processor
            .process_all_accounts()
            .await
            .expect("per-candidate errors are logged, not surfaced by the pass");
        assert!(
            storage.get_submit_state_calls().is_empty(),
            "a refused promotion must not have written state",
        );
        assert!(
            storage.get_pull_delta_proposal_calls().is_empty(),
            "trailing proposal cleanup must not run after a refused promotion",
        );
    }

    #[tokio::test]
    async fn superseded_candidate_promotion_is_a_clean_skip() {
        let account_id = "0xtest_account";
        let (storage, network, metadata) = promotion_mocks(account_id);
        let storage = Arc::new(
            storage.with_promote_candidate(Ok(crate::storage::PromoteWrite::NotCandidate)),
        );

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(network),
            Arc::new(metadata),
        );
        let processor = fenced_processor(state, CanonicalizationConfig::default());

        processor
            .process_all_accounts()
            .await
            .expect("a superseded candidate is skipped without error");
        assert!(
            storage.get_pull_delta_proposal_calls().is_empty(),
            "another owner finished this candidate; its cleanup is not ours to run",
        );
    }

    #[tokio::test]
    async fn superseded_discard_deletes_nothing_and_skips_cleanup() {
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status =
            DeltaStatus::candidate_with_retry("2024-01-01T00:00:00Z".to_string(), 17);

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_discard_candidate(Ok(crate::storage::CanonicalWrite::NotCandidate)),
        );
        let network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Err("Verification failed".to_string()));
        let metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))));

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(network),
            Arc::new(metadata),
        );
        // Retention disabled: this test pins the refused-DISCARD path;
        // the retained twin below pins the refused-retain path.
        let processor = fenced_processor(
            state,
            CanonicalizationConfig::new(10, 18).with_retained_ttl_seconds(0),
        );

        processor
            .process_all_accounts()
            .await
            .expect("a superseded discard is skipped without error");
        assert!(
            storage.get_delete_delta_calls().is_empty(),
            "the storage-level discard already refused; nothing may be deleted",
        );
        assert!(
            storage.get_pull_delta_proposal_calls().is_empty(),
            "proposal cleanup must not follow a refused discard",
        );
    }

    #[tokio::test]
    async fn superseded_retain_writes_nothing_and_skips_cleanup() {
        // The retain-path twin of the refused-discard test above: when
        // the fenced status flip refuses (row no longer a candidate),
        // neither the flag clear nor the proposal cleanup may follow.
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status =
            DeltaStatus::candidate_with_retry("2024-01-01T00:00:00Z".to_string(), 17);

        let storage = Arc::new(
            MockStorageBackend::new()
                .with_pull_deltas_after(Ok(vec![candidate]))
                .with_pull_state(Ok(create_test_state(account_id)))
                .with_update_candidate_status(Ok(crate::storage::CanonicalWrite::NotCandidate)),
        );
        let network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_commitment(Err("Verification failed".to_string()));
        let metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))));
        let metadata = Arc::new(metadata);

        let state =
            create_test_app_state_with_mocks(storage.clone(), Arc::new(network), metadata.clone());
        let processor = fenced_processor(state, CanonicalizationConfig::new(10, 18));

        processor
            .process_all_accounts()
            .await
            .expect("a refused retain is skipped without error");
        assert!(storage.get_delete_delta_calls().is_empty());
        assert!(
            storage.get_pull_delta_proposal_calls().is_empty(),
            "proposal cleanup must not follow a refused retain",
        );
        assert!(
            metadata.get_set_calls().is_empty(),
            "the flag clear must not follow a refused retain",
        );
    }
}
