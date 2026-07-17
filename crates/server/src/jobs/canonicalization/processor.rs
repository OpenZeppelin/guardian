use std::sync::Arc;

use crate::canonicalization::CanonicalizationConfig;
use crate::coordination::{AlwaysLeader, CANONICALIZATION_LEASE, LeaderElector, Lease};
use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::error::{GuardianError, Result};
use crate::network::StateVerification;
use crate::state::AppState;
use crate::state_object::StateObject;
use crate::storage::{CandidatePromotion, CanonicalWrite, LeaseFence, PromoteWrite};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

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

/// True when an on-chain commitment is the empty-word digest the chain
/// node reports for an account it has never seen — i.e. the account's
/// first transaction has not landed yet. This is the not-yet-landed case,
/// categorically distinct from divergence (where the account moved to a
/// *different*, non-zero state past the candidate's base).
fn is_absent_on_chain(on_chain: &str) -> bool {
    let digest = on_chain.strip_prefix("0x").unwrap_or(on_chain);
    !digest.is_empty() && digest.bytes().all(|b| b == b'0')
}

struct DeltasProcessorBase {
    state: AppState,
    pass: PassLease,
    max_retries: u32,
    submission_grace_period_seconds: u64,
    divergence_confirmations: u32,
    max_concurrent_accounts: usize,
}

impl DeltasProcessorBase {
    /// Fence descriptor attached to every custody-state write: the canonical
    /// promotion, the discard, and the retry / divergence-streak status
    /// updates. Fenced backends (Postgres) validate it against the lease row
    /// inside the same transaction as the write and hold the row locked until
    /// commit, so a superseded holder can never commit a custody transition —
    /// there is no check-then-write window. Backends additionally require the
    /// target delta to still be a candidate, so a delayed stale write can
    /// neither demote nor delete a delta another owner already promoted.
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

    async fn process_all_accounts(&self) -> Result<PassSummary> {
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

        // Accounts overlap with bounded concurrency — the per-account cost
        // is dominated by the Miden RPC round trip, so a sequential pass
        // wastes almost its entire wall clock waiting. Candidates within
        // an account stay strictly sequential (nonce order) inside
        // `process_account`, and every custody write remains individually
        // fenced, so correctness does not depend on this bound.
        let accounts = account_ids.len();
        let failed_accounts = futures::stream::iter(account_ids)
            .map(|account_id| async move { self.process_account_absorbing(&account_id).await })
            .buffer_unordered(self.max_concurrent_accounts.max(1))
            .fold(0, |failed, account_failed| async move {
                failed + usize::from(account_failed)
            })
            .await;

        if self.pass.cancel.is_cancelled() {
            tracing::warn!(
                "Canonicalization pass cancelled (lease lost); remaining accounts skipped"
            );
        }

        Ok(PassSummary {
            accounts,
            failed_accounts,
            cancelled: self.pass.cancel.is_cancelled(),
        })
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

    async fn process_account(&self, account_id: &str) -> Result<()> {
        let _account_metadata = self
            .state
            .metadata
            .get(account_id)
            .await
            .map_err(|e| GuardianError::StorageError(format!("Failed to get metadata: {e}")))?
            .ok_or_else(|| GuardianError::InvalidInput("Account metadata not found".to_string()))?;

        let storage_backend = self.state.storage.clone();

        let candidates = storage_backend
            .pull_candidate_deltas(account_id)
            .await
            .map_err(|e| GuardianError::StorageError(format!("Failed to pull deltas: {e}")))?;

        tracing::info!(
            account_id = %account_id,
            candidates = candidates.len(),
            "Processing delta candidates"
        );
        metrics::counter!(crate::metrics::names::CANONICALIZATION_DELTAS_FETCHED_TOTAL)
            .increment(candidates.len() as u64);

        let mut first_error = None;
        for delta in candidates {
            if self.pass.cancel.is_cancelled() {
                tracing::warn!(
                    account_id = %account_id,
                    "Canonicalization pass cancelled (lease lost); stopping before next candidate"
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
            let client = &self.state.network_client;
            client
                .apply_delta(&current_state.state_json, &delta.delta_payload)
                .map_err(GuardianError::InvalidDelta)?
        };

        let verify_result = {
            let client = &self.state.network_client;
            client
                .verify_state(&delta.account_id, &new_state_json)
                .await
        };

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
                self.canonicalize_verified_delta(delta, new_state_json, recomputed_commitment)
                    .await
            }
            // The chain node has never seen this account: an all-zero
            // on-chain commitment means the account's first transaction has
            // not landed yet, not that it advanced past the candidate's base.
            // A first-nonce candidate is anchored to the account's seed
            // commitment (never zero), so it can never match the base branch
            // below — treat it as not-yet-landed (defer/retry) and clear any
            // divergence streak a lagging node may have started.
            Ok(StateVerification::Mismatch { on_chain }) if is_absent_on_chain(&on_chain) => {
                let delta = self.reset_divergence_streak(delta).await?;
                self.handle_unverified_candidate(
                    delta,
                    &format!("account not yet on chain (on-chain commitment {on_chain})"),
                )
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
            // transaction simply has not landed yet. Defer within the
            // grace period, then consume retry budget, as before. This
            // read also proves any earlier diverged observation was
            // stale, so the divergence streak starts over.
            Ok(StateVerification::Mismatch { on_chain }) => {
                let delta = self.reset_divergence_streak(delta).await?;
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

        tracing::warn!(
            account_id = %delta.account_id,
            nonce = delta.nonce,
            on_chain = %on_chain,
            prev_commitment = %delta.prev_commitment,
            observations,
            "Account advanced past candidate's base state on-chain; discarding \
             unsatisfiable candidate and releasing the account"
        );

        let now = self.state.clock.now().to_rfc3339();
        match self.remove_candidate(&delta, &now).await? {
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
            tracing::warn!(
                account_id = %delta.account_id,
                nonce = delta.nonce,
                retries = new_retry,
                max_retries = self.max_retries,
                error = %reason,
                "Delta verification failed after max retries, discarding"
            );

            match self.remove_candidate(&delta, &now).await? {
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

    /// Delete a candidate that can never be canonicalized, delete its
    /// matching proposal, and release the account's pending-candidate lock.
    /// The delete, the fence validation, and the conditional flag clear
    /// commit as one fenced storage write; the delete only touches a row
    /// still in candidate status, so a stale discard can never remove a
    /// delta another owner promoted to canonical.
    async fn remove_candidate(&self, delta: &DeltaObject, now: &str) -> Result<CanonicalWrite> {
        let storage_backend = self.state.storage.clone();

        let outcome = storage_backend
            .discard_candidate(
                self.state.metadata.as_ref(),
                &delta.account_id,
                delta.nonce,
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
        // stale intent.
        let proposal_id = {
            let client = &self.state.network_client;
            match client.delta_proposal_id(&delta.account_id, delta.nonce, &delta.delta_payload) {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::warn!(
                        account_id = %delta.account_id,
                        nonce = delta.nonce,
                        error = %e,
                        "Could not derive proposal id for discarded delta; \
                         its proposal may remain stranded as pending"
                    );
                    None
                }
            }
        };
        if let Some(ref id) = proposal_id
            && let Ok(_existing_proposal) = storage_backend
                .pull_delta_proposal(&delta.account_id, id)
                .await
        {
            tracing::warn!(
                account_id = %delta.account_id,
                proposal_id = %id,
                "Deleting matching proposal as its delta was discarded"
            );
            if let Err(e) = storage_backend
                .delete_delta_proposal(&delta.account_id, id)
                .await
            {
                tracing::warn!(
                    account_id = %delta.account_id,
                    proposal_id = %id,
                    error = %e,
                    "Failed to delete proposal after discard, but continuing"
                );
            }
        }

        Ok(CanonicalWrite::Applied)
    }

    async fn canonicalize_verified_delta(
        &self,
        delta: DeltaObject,
        new_state_json: serde_json::Value,
        verified_commitment: String,
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

        // The typed `metadata` blob is populated at push time; this
        // path just flips the status.
        let mut canonical_delta = delta.clone();
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

        record_candidate_outcome(crate::metrics::labels::CandidateOutcome::Canonicalized);
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
        let pass = PassLease::single_process();
        Self::with_lease(state, config, pass.leader, pass.lease, pass.cancel)
    }

    /// Lease-bound processor used by the multi-replica worker: writes are fenced
    /// by `leader`/`lease` and the pass aborts when `cancel` is tripped.
    pub fn with_lease(
        state: AppState,
        config: CanonicalizationConfig,
        leader: Arc<dyn LeaderElector>,
        lease: Lease,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            base: DeltasProcessorBase {
                state,
                pass: PassLease {
                    leader,
                    lease,
                    cancel,
                },
                max_retries: config.max_retries,
                submission_grace_period_seconds: config.submission_grace_period_seconds,
                divergence_confirmations: config.divergence_confirmations,
                max_concurrent_accounts: config.max_concurrent_accounts,
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
                max_retries: u32::MAX, // Test processor doesn't discard on retries
                submission_grace_period_seconds: 0,
                divergence_confirmations: u32::MAX, // ...nor on divergence
                max_concurrent_accounts: 1,         // ...and stays deterministic
            },
        }
    }
}

#[async_trait]
impl Processor for TestDeltasProcessor {
    async fn process_all_accounts(&self) -> Result<PassSummary> {
        self.base.process_all_accounts().await
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

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_state(Ok(StateVerification::Match))
            .with_should_update_auth(Ok(None));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
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
        assert_eq!(storage.get_submit_state_calls().len(), 1);
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
            .with_verify_state(Ok(StateVerification::Match))
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
            .with_verify_state(Ok(StateVerification::Match))
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
            .with_verify_state(Err("Verification failed".to_string()));

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
            .with_verify_state(Err("Verification failed".to_string()));

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
            .with_verify_state(Ok(StateVerification::Mismatch {
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
            .with_verify_state(Ok(StateVerification::Mismatch {
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
    async fn test_diverged_candidate_confirmed_discards_and_releases() {
        // Second consecutive diverged observation reaches the default
        // confirmation threshold (2): the unsatisfiable candidate is deleted
        // and the account lock released immediately, well within the grace
        // period.
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.status = DeltaStatus::Candidate {
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            retry_count: 0,
            divergence_count: 1,
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
            .with_verify_state(Ok(StateVerification::Mismatch {
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

        assert_eq!(
            storage.get_delete_delta_calls(),
            vec![(account_id.to_string(), 1)]
        );
        assert!(storage.get_update_delta_status_calls().is_empty());
        assert!(
            metadata
                .get_set_calls()
                .iter()
                .any(|m| !m.has_pending_candidate)
        );
    }

    #[tokio::test]
    async fn test_divergence_confirmations_of_one_discards_immediately() {
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
            .with_verify_state(Ok(StateVerification::Mismatch {
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

        assert_eq!(
            storage.get_delete_delta_calls(),
            vec![(account_id.to_string(), 1)]
        );
        assert!(storage.get_update_delta_status_calls().is_empty());
    }

    #[tokio::test]
    async fn test_absent_on_chain_account_is_not_discarded_as_diverged() {
        // A first transaction that has not landed yet reads the empty-word
        // digest on-chain: the node returns an all-zero commitment for an
        // account it has never seen. That is the not-yet-landed case, never
        // divergence — the account has not advanced past its base, it simply
        // is not there yet. Even with divergence_confirmations == 1 (which
        // would discard a genuine divergence on the first observation) the
        // candidate must survive and take the retry path.
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
            .with_verify_state(Ok(StateVerification::Mismatch {
                on_chain: "0x0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string(),
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
            .with_verify_state(Ok(StateVerification::Mismatch {
                on_chain: "0xsome_other_commitment".to_string(),
            }))
            // tick 2: back at the candidate's base
            .with_verify_state(Ok(StateVerification::Mismatch {
                on_chain: "prev_commitment".to_string(),
            }))
            // tick 1: diverged
            .with_verify_state(Ok(StateVerification::Mismatch {
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

        let mock_storage = MockStorageBackend::new()
            .with_pull_deltas_after(Ok(vec![candidate.clone()]))
            .with_pull_state(Ok(create_test_state(account_id)));

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_state(Err("Verification failed".to_string()));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(())); // For clearing has_pending_candidate

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        // max_retries = 10, so retry_count 9 + 1 = 10 >= 10, will discard
        let config = CanonicalizationConfig::new(10, 18);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
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
            .with_verify_state(Ok(StateVerification::Match))
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
            .with_verify_state(Ok(StateVerification::Match))
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
            .with_verify_state(Ok(StateVerification::Match))
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
            .with_verify_state(Ok(StateVerification::Match))
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
            .with_verify_state(Ok(StateVerification::Match))
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
        DeltasProcessor::with_lease(
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
                .with_verify_state(Ok(StateVerification::Match))
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
        let processor = DeltasProcessor::with_lease(
            state,
            CanonicalizationConfig::default(),
            pass.leader,
            pass.lease,
            cancel,
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
            .with_verify_state(Ok(StateVerification::Match))
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
            .with_verify_state(Err("Verification failed".to_string()));
        let metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))));

        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(network),
            Arc::new(metadata),
        );
        let processor = fenced_processor(state, CanonicalizationConfig::new(10, 18));

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
}
