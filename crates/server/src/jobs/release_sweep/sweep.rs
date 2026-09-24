//! Per-account release detection: the probes, the two detectors, and
//! the cross-pass state (rotation cursor, confirmation streaks, cached
//! proposal post-states). The worker in `worker.rs` decides *when* an
//! account is visited; everything here decides *what* a visit does.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::error::{GuardianError, Result};
use crate::metadata::AccountMetadata;
use crate::metrics::labels::ReleaseSweepOutcome;
use crate::network::{OnChainGuardianBinding, RpcReadMode, StateVerification};
use crate::services::release_on_switch::{
    ReleaseEvidence, ReleaseWrite, own_guardian_commitment, release_switched_account,
};
use crate::state::AppState;
use crate::state_object::StateObject;
use crate::storage::{GlobalProposalCursor, ProposalRecord};

/// Proposal type tag of a guardian switch, as the multisig SDKs write it.
pub const SWITCH_GUARDIAN_PROPOSAL_TYPE: &str = "switch_guardian";

/// Page size used when walking the global pending-proposal feed for the
/// hot set.
const PROPOSAL_FEED_PAGE: u32 = 100;

/// What one pass (a rotation or a hot pass) did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepSummary {
    pub accounts: usize,
    pub failed_accounts: usize,
    pub cancelled: bool,
}

/// A pending switch proposal's post-state, computed once from the stored
/// base it chains from and reused on every later probe until the
/// proposal is resolved or the base moves.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PrecomputedSwitch {
    prev_commitment: String,
    post_commitment: String,
    guardian_commitment: Option<String>,
}

/// Replica-local cross-pass state. A failover restarts the rotation and
/// the streaks, which only delays a release by the confirmation count.
#[derive(Default)]
pub struct SweepState {
    /// Rotation cursor: the last account id visited in the current walk.
    cursor: Mutex<Option<String>>,
    /// account_id → (observed foreign guardian commitment, consecutive
    /// observations). Cleared on any observation that is not that key.
    streaks: Mutex<BTreeMap<String, (String, u32)>>,
    /// proposal id → post-state of a pending switch proposal.
    precomputed: Mutex<HashMap<String, PrecomputedSwitch>>,
}

impl SweepState {
    pub fn cursor(&self) -> Option<String> {
        self.cursor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub fn set_cursor(&self, cursor: Option<String>) {
        *self
            .cursor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = cursor;
    }

    /// Accounts with an open confirmation streak, in id order.
    pub fn pending_confirmations(&self) -> Vec<String> {
        self.streaks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    /// Record one observation of `guardian_commitment` (a key that is not
    /// this server's) and return the streak length it now has. A
    /// different foreign key than the one on record restarts the streak
    /// at one: confirmations must agree on *which* key the chain shows.
    fn record_foreign_observation(&self, account_id: &str, guardian_commitment: &str) -> u32 {
        let mut streaks = self
            .streaks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let entry = streaks
            .entry(account_id.to_string())
            .or_insert_with(|| (guardian_commitment.to_string(), 0));
        if entry.0 != guardian_commitment {
            *entry = (guardian_commitment.to_string(), 0);
        }
        entry.1 = entry.1.saturating_add(1);
        entry.1
    }

    fn clear_streak(&self, account_id: &str) {
        self.streaks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(account_id);
    }

    pub fn streak(&self, account_id: &str) -> Option<u32> {
        self.streaks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(account_id)
            .map(|(_, count)| *count)
    }

    fn precomputed(&self, proposal_id: &str, prev_commitment: &str) -> Option<PrecomputedSwitch> {
        self.precomputed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(proposal_id)
            .filter(|entry| entry.prev_commitment == prev_commitment)
            .cloned()
    }

    fn remember(&self, proposal_id: &str, entry: PrecomputedSwitch) {
        self.precomputed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(proposal_id.to_string(), entry);
    }

    fn forget(&self, proposal_id: &str) {
        self.precomputed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(proposal_id);
    }
}

fn record_outcome(outcome: ReleaseSweepOutcome) {
    metrics::counter!(
        crate::metrics::names::RELEASE_SWEEP_ACCOUNTS_TOTAL,
        crate::metrics::names::LABEL_OUTCOME => outcome.as_str()
    )
    .increment(1);
}

/// One pending switch proposal whose precomputed post-state is exactly
/// where the chain sits now.
struct MatchedProposal {
    /// The id as the storage backend keys it (the filesystem backend
    /// keeps it unprefixed), for the finalizing delete.
    storage_id: String,
    /// The id as the API and audit trail spell it (`0x`-prefixed).
    proposal_id: String,
    guardian_commitment: Option<String>,
}

/// Proposal ids are `0x`-prefixed on the wire; the filesystem backend
/// keys them without the prefix.
fn canonical_proposal_id(storage_id: &str) -> String {
    format!("0x{}", storage_id.trim_start_matches("0x"))
}

/// The release detector over one server's accounts. Cheap to construct;
/// the worker builds one per lease tenure around the shared
/// [`SweepState`].
pub struct ReleaseSweeper {
    state: AppState,
    confirmations: u32,
    sweep: Arc<SweepState>,
    cancel: CancellationToken,
}

impl ReleaseSweeper {
    pub fn new(
        state: AppState,
        confirmations: u32,
        sweep: Arc<SweepState>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            state,
            confirmations,
            sweep,
            cancel,
        }
    }

    pub fn sweep_state(&self) -> &Arc<SweepState> {
        &self.sweep
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Next page of the rotation walk.
    pub async fn list_page(&self, after: Option<&str>, limit: u32) -> Result<Vec<AccountMetadata>> {
        self.state
            .metadata
            .list_release_sweep_page(after, limit)
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to list sweepable accounts: {e}"))
            })
    }

    /// Number of accounts this server holds, for pacing the rotation.
    pub async fn fleet_size(&self) -> Result<usize> {
        self.state
            .metadata
            .list()
            .await
            .map(|ids| ids.len())
            .map_err(|e| GuardianError::StorageError(format!("Failed to count accounts: {e}")))
    }

    /// The accounts that deserve attention sooner than the next rotation:
    /// those with an open confirmation streak, and those carrying a
    /// pending `switch_guardian` proposal (a switch that may have
    /// executed elsewhere any moment). Resolved by id; rows that no
    /// longer qualify drop out (and drop their streak).
    pub async fn hot_targets(&self) -> Result<Vec<AccountMetadata>> {
        let mut ids: Vec<String> = self.sweep.pending_confirmations();
        let mut seen: HashSet<String> = ids.iter().cloned().collect();

        let mut cursor: Option<GlobalProposalCursor> = None;
        loop {
            let page = self
                .state
                .storage
                .list_global_proposals_paged(PROPOSAL_FEED_PAGE, cursor.take())
                .await
                .map_err(|e| {
                    GuardianError::StorageError(format!("Failed to list pending proposals: {e}"))
                })?;
            let page_len = page.len();
            for record in &page {
                if record.proposal.proposal_type() == Some(SWITCH_GUARDIAN_PROPOSAL_TYPE)
                    && seen.insert(record.account_id.clone())
                {
                    ids.push(record.account_id.clone());
                }
            }
            if page_len < PROPOSAL_FEED_PAGE as usize {
                break;
            }
            cursor = page.last().and_then(proposal_feed_cursor);
            if cursor.is_none() {
                break;
            }
        }

        let mut targets = Vec::with_capacity(ids.len());
        for account_id in ids {
            match self.state.metadata.get(&account_id).await {
                Ok(Some(metadata)) if metadata.released_at.is_none() => targets.push(metadata),
                Ok(_) => self.sweep.clear_streak(&account_id),
                Err(e) => tracing::warn!(
                    account_id = %account_id,
                    error = %e,
                    "Failed to reload a hot release-sweep target; retrying next pass"
                ),
            }
        }
        Ok(targets)
    }

    /// Visit every hot target once.
    pub async fn hot_pass(&self) -> Result<SweepSummary> {
        let targets = self.hot_targets().await?;
        let mut summary = SweepSummary::default();
        for metadata in &targets {
            if self.cancel.is_cancelled() {
                summary.cancelled = true;
                break;
            }
            summary.accounts += 1;
            summary.failed_accounts += usize::from(self.visit_absorbing(metadata).await);
        }
        Ok(summary)
    }

    /// Visit one account, absorbing the error into a `true` return so a
    /// pass can keep going and count it.
    pub async fn visit_absorbing(&self, metadata: &AccountMetadata) -> bool {
        match self.visit(metadata).await {
            Ok(()) => false,
            Err(e) => {
                tracing::error!(
                    account_id = %metadata.account_id,
                    error = %e,
                    "Release sweep failed for account"
                );
                true
            }
        }
    }

    /// Sweep one account: the cheap commitment probe, then the detectors
    /// only when the chain moved past the stored base.
    pub async fn visit(&self, metadata: &AccountMetadata) -> Result<()> {
        let account_id = metadata.account_id.as_str();

        // The store already filters these; the checks stay for rows
        // reloaded by id (hot targets) and for defense in depth. EVM
        // accounts have no on-chain guardian binding; a row released
        // between listing and visiting has nothing left to decide.
        if metadata.network_config.is_evm() || metadata.released_at.is_some() {
            self.sweep.clear_streak(account_id);
            return Ok(());
        }
        // A candidate in flight belongs to the push path: it either
        // canonicalizes (and the hook releases on a switch) or resolves
        // to a state this sweep sees on a later visit. No observation
        // is recorded either way.
        if metadata.has_pending_candidate {
            tracing::debug!(
                event = "release_sweep_skipped",
                reason = "pending_candidate",
                account_id = %account_id,
                "Candidate in flight; the push path owns this account for now"
            );
            return Ok(());
        }

        let stored = self
            .state
            .storage
            .pull_state(account_id)
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to get current state: {e}"))
            })?;

        let verification = self
            .state
            .network_client
            .verify_commitment(account_id, &stored.commitment, RpcReadMode::SingleAttempt)
            .await;
        match verification {
            // The chain sits at the stored state (or has none yet): the
            // stored guardian key is the on-chain one, and it is ours.
            // Not counted — this is every healthy account, every pass.
            Ok(StateVerification::Match) | Ok(StateVerification::Absent) => {
                self.sweep.clear_streak(account_id);
                tracing::debug!(
                    event = "release_sweep_deferred",
                    reason = "chain_at_stored_base",
                    account_id = %account_id,
                    "Chain at the stored base; guardian binding unchanged"
                );
                Ok(())
            }
            Err(e) => {
                tracing::info!(
                    event = "release_sweep_deferred",
                    reason = "chain_probe_unavailable",
                    account_id = %account_id,
                    error = %e,
                    "Chain probe unavailable; deferring release check"
                );
                record_outcome(ReleaseSweepOutcome::ProbeFailed);
                Ok(())
            }
            Ok(StateVerification::Mismatch { on_chain }) => {
                self.visit_diverged(metadata, &stored, &on_chain).await
            }
        }
    }

    /// The chain moved past the stored base. Either a transaction this
    /// server acknowledged landed without the store catching up (the
    /// guardian key is still ours — issue #345 territory, not a switch),
    /// or the one transaction the account can execute without this
    /// server's signature ran: a guardian key rotation. A pending switch
    /// proposal whose post-state is exactly the chain head proves the
    /// latter for any account; published storage tells the two apart
    /// for public accounts.
    async fn visit_diverged(
        &self,
        metadata: &AccountMetadata,
        stored: &StateObject,
        on_chain: &str,
    ) -> Result<()> {
        let account_id = metadata.account_id.as_str();
        let own_commitment = own_guardian_commitment(&self.state, metadata);

        // Detector 1: proposal match. An exact commitment match is proof
        // — a lagging node cannot invent the post-switch commitment — so
        // it needs no confirmation streak.
        if let Some(matched) = self
            .match_pending_switch_proposal(metadata, stored, on_chain)
            .await?
        {
            return self
                .release_on_proposal_match(metadata, stored, on_chain, &own_commitment, matched)
                .await;
        }

        // Detector 2: published storage.
        let binding = self
            .state
            .network_client
            .fetch_on_chain_guardian_binding(account_id, RpcReadMode::SingleAttempt)
            .await;
        let (on_chain_commitment, guardian_commitment) = match binding {
            Err(e) => {
                tracing::info!(
                    event = "release_sweep_deferred",
                    reason = "binding_read_unavailable",
                    account_id = %account_id,
                    on_chain = %on_chain,
                    error = %e,
                    "Chain moved past the stored base but the guardian binding \
                     could not be read; deferring"
                );
                record_outcome(ReleaseSweepOutcome::ProbeFailed);
                return Ok(());
            }
            Ok(OnChainGuardianBinding::Opaque) => {
                tracing::info!(
                    event = "release_sweep_deferred",
                    reason = "storage_opaque",
                    account_id = %account_id,
                    stored_commitment = %stored.commitment,
                    on_chain = %on_chain,
                    "Chain moved past the stored base, no pending switch proposal \
                     matches it, and the account does not publish its storage; the \
                     guardian binding cannot be verified from chain"
                );
                record_outcome(ReleaseSweepOutcome::StorageOpaque);
                return Ok(());
            }
            Ok(OnChainGuardianBinding::Visible {
                on_chain_commitment,
                guardian_commitment,
            }) => (on_chain_commitment, guardian_commitment),
        };

        // The two reads disagree about where the chain is: the storage
        // read says the chain is at the stored base after all, so the
        // first probe was the odd one out (a lagging node). No
        // observation either way.
        if on_chain_commitment == stored.commitment {
            self.sweep.clear_streak(account_id);
            tracing::debug!(
                event = "release_sweep_deferred",
                reason = "chain_at_stored_base",
                account_id = %account_id,
                "Storage read shows the chain at the stored base; probe reads disagreed"
            );
            return Ok(());
        }

        let new_guardian_commitment = match guardian_commitment {
            None => {
                self.sweep.clear_streak(account_id);
                tracing::info!(
                    event = "release_sweep_deferred",
                    reason = "no_guardian_binding",
                    account_id = %account_id,
                    on_chain = %on_chain_commitment,
                    "Published storage carries no guardian binding; absence is not a switch"
                );
                record_outcome(ReleaseSweepOutcome::NoBinding);
                return Ok(());
            }
            Some(commitment) if commitment == own_commitment => {
                self.sweep.clear_streak(account_id);
                tracing::info!(
                    event = "release_sweep_deferred",
                    reason = "guardian_still_bound",
                    account_id = %account_id,
                    stored_commitment = %stored.commitment,
                    on_chain = %on_chain_commitment,
                    "Chain moved past the stored base but the account is still \
                     bound to this guardian (stored state lagging the chain)"
                );
                record_outcome(ReleaseSweepOutcome::StillBound);
                return Ok(());
            }
            Some(commitment) => commitment,
        };

        let streak = self
            .sweep
            .record_foreign_observation(account_id, &new_guardian_commitment);
        if streak < self.confirmations {
            tracing::info!(
                event = "release_sweep_confirming",
                account_id = %account_id,
                new_guardian_commitment = %new_guardian_commitment,
                observations = streak,
                confirmations = self.confirmations,
                "On-chain guardian key differs from this server's; awaiting confirmation"
            );
            record_outcome(ReleaseSweepOutcome::Confirming);
            return Ok(());
        }

        if !self.stored_base_unchanged(account_id, stored).await? {
            return Ok(());
        }
        self.release(
            metadata,
            &new_guardian_commitment,
            ReleaseEvidence::ChainSweep {
                on_chain_commitment: &on_chain_commitment,
                stored_commitment: &stored.commitment,
            },
        )
        .await
    }

    /// Find a pending `switch_guardian` proposal that chains from the
    /// stored base and whose post-state commitment is exactly `on_chain`.
    /// The post-state is computed with the same `apply_delta` the
    /// canonicalization pass uses and cached per proposal; a reconstruction
    /// that fails only rules that proposal out.
    async fn match_pending_switch_proposal(
        &self,
        metadata: &AccountMetadata,
        stored: &StateObject,
        on_chain: &str,
    ) -> Result<Option<MatchedProposal>> {
        let account_id = metadata.account_id.as_str();
        let proposals = self
            .state
            .storage
            .pull_pending_proposals(account_id)
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to pull pending proposals: {e}"))
            })?;
        for record in proposals {
            if record.proposal.proposal_type() != Some(SWITCH_GUARDIAN_PROPOSAL_TYPE)
                || record.proposal.prev_commitment != stored.commitment
            {
                continue;
            }
            let entry = match self
                .sweep
                .precomputed(&record.commitment, &stored.commitment)
            {
                Some(entry) => entry,
                None => match self.precompute_switch(&record, stored).await {
                    Some(entry) => {
                        self.sweep.remember(&record.commitment, entry.clone());
                        entry
                    }
                    None => continue,
                },
            };
            if entry.post_commitment == on_chain {
                return Ok(Some(MatchedProposal {
                    storage_id: record.commitment.clone(),
                    proposal_id: canonical_proposal_id(&record.commitment),
                    guardian_commitment: entry.guardian_commitment,
                }));
            }
        }
        Ok(None)
    }

    async fn precompute_switch(
        &self,
        record: &ProposalRecord,
        stored: &StateObject,
    ) -> Option<PrecomputedSwitch> {
        let Some(tx_summary) = record.proposal.delta_payload.get("tx_summary").cloned() else {
            tracing::warn!(
                account_id = %record.account_id,
                proposal_id = %record.commitment,
                "Pending switch proposal carries no tx_summary; cannot precompute its post-state"
            );
            return None;
        };
        let client = self.state.network_client.clone();
        let prev_state_json = stored.state_json.clone();
        let applied = crate::network::reconstructor()
            .run_background(move || client.apply_delta(&prev_state_json, &tx_summary))
            .await;
        let (post_state, post_commitment) = match applied {
            Ok(applied) => applied,
            Err(e) => {
                tracing::info!(
                    account_id = %record.account_id,
                    proposal_id = %record.commitment,
                    error = %GuardianError::from(e),
                    "Pending switch proposal does not apply to the stored base; \
                     it cannot serve as release evidence"
                );
                return None;
            }
        };
        let guardian_commitment = match self
            .state
            .network_client
            .extract_guardian_commitment(&post_state)
        {
            Ok(guardian) => guardian,
            Err(e) => {
                tracing::warn!(
                    account_id = %record.account_id,
                    proposal_id = %record.commitment,
                    error = %e,
                    "Could not inspect the guardian key of a switch proposal's post-state"
                );
                return None;
            }
        };
        Some(PrecomputedSwitch {
            prev_commitment: stored.commitment.clone(),
            post_commitment,
            guardian_commitment,
        })
    }

    async fn release_on_proposal_match(
        &self,
        metadata: &AccountMetadata,
        stored: &StateObject,
        on_chain: &str,
        own_commitment: &str,
        matched: MatchedProposal,
    ) -> Result<()> {
        let account_id = metadata.account_id.as_str();
        let new_guardian_commitment = match matched.guardian_commitment {
            // A "switch" to this server's own key (or to no key) executed:
            // the account is still bound here. Nothing to release; the
            // proposal is left for the push path / operator.
            Some(commitment) if commitment != own_commitment => commitment,
            other => {
                self.sweep.clear_streak(account_id);
                tracing::info!(
                    event = "release_sweep_deferred",
                    reason = "guardian_still_bound",
                    account_id = %account_id,
                    proposal_id = %matched.proposal_id,
                    on_chain = %on_chain,
                    guardian = ?other,
                    "Pending switch proposal executed but its post-state keeps this \
                     server's guardian key"
                );
                record_outcome(ReleaseSweepOutcome::StillBound);
                return Ok(());
            }
        };

        if !self.stored_base_unchanged(account_id, stored).await? {
            return Ok(());
        }
        let outcome = self
            .release(
                metadata,
                &new_guardian_commitment,
                ReleaseEvidence::ProposalMatch {
                    proposal_id: &matched.proposal_id,
                    on_chain_commitment: on_chain,
                    stored_commitment: &stored.commitment,
                },
            )
            .await;
        // The proposal is finalized the moment the chain proved it
        // executed: resolve it like canonicalization resolves a matching
        // proposal, so it stops lingering as pending.
        self.finalize_proposal(account_id, &matched.storage_id, &matched.proposal_id)
            .await;
        outcome
    }

    /// Re-check the stored base right before writing: a `/configure`
    /// that re-onboarded the account meanwhile replaced the stored state
    /// (and cleared any release), so evidence gathered against the old
    /// base no longer applies. The next visit re-evaluates from the new
    /// base.
    async fn stored_base_unchanged(&self, account_id: &str, stored: &StateObject) -> Result<bool> {
        let current = self
            .state
            .storage
            .pull_state(account_id)
            .await
            .map_err(|e| GuardianError::StorageError(format!("Failed to re-read state: {e}")))?;
        if current.commitment != stored.commitment {
            self.sweep.clear_streak(account_id);
            tracing::info!(
                event = "release_sweep_deferred",
                reason = "stored_base_moved",
                account_id = %account_id,
                "Stored state moved during the sweep; re-evaluating from the new base next pass"
            );
            return Ok(false);
        }
        Ok(true)
    }

    async fn release(
        &self,
        metadata: &AccountMetadata,
        new_guardian_commitment: &str,
        evidence: ReleaseEvidence<'_>,
    ) -> Result<()> {
        let account_id = metadata.account_id.as_str();
        let detected_by = match &evidence {
            ReleaseEvidence::ProposalMatch { .. } => "proposal_match",
            _ => "chain_sweep",
        };
        match release_switched_account(&self.state, metadata, new_guardian_commitment, evidence)
            .await
        {
            ReleaseWrite::Released => {
                self.sweep.clear_streak(account_id);
                tracing::info!(
                    event = "release_sweep_released",
                    account_id = %account_id,
                    detected_by,
                    new_guardian_commitment = %new_guardian_commitment,
                    "Released an account whose guardian switch never reached the push path"
                );
                record_outcome(ReleaseSweepOutcome::Released);
                Ok(())
            }
            ReleaseWrite::AlreadyReleased => {
                self.sweep.clear_streak(account_id);
                Ok(())
            }
            // The streak is kept: the observation stands, only the write
            // failed, so the next pass retries the release directly.
            ReleaseWrite::Failed => Err(GuardianError::StorageError(format!(
                "Failed to persist release for switched account {account_id}"
            ))),
        }
    }

    async fn finalize_proposal(&self, account_id: &str, storage_id: &str, proposal_id: &str) {
        self.sweep.forget(storage_id);
        metrics::counter!(
            crate::metrics::names::PROPOSALS_TOTAL,
            crate::metrics::names::LABEL_EVENT =>
                crate::metrics::labels::ProposalEvent::Finalized.as_str()
        )
        .increment(1);
        match self
            .state
            .storage
            .delete_delta_proposal(account_id, storage_id)
            .await
        {
            Ok(()) => tracing::info!(
                event = "release_sweep_proposal_finalized",
                account_id = %account_id,
                proposal_id = %proposal_id,
                "Resolved the pending switch proposal the chain proved executed"
            ),
            Err(e) => tracing::warn!(
                account_id = %account_id,
                proposal_id = %proposal_id,
                error = %e,
                "Failed to delete the executed switch proposal; it stays pending"
            ),
        }
    }
}

/// Keyset cursor after the last record of a global proposal page, or
/// `None` when the record's originating timestamp is unreadable (ends the
/// walk rather than looping).
fn proposal_feed_cursor(record: &ProposalRecord) -> Option<GlobalProposalCursor> {
    let timestamp = chrono::DateTime::parse_from_rfc3339(record.proposal.status.timestamp())
        .ok()?
        .with_timezone(&chrono::Utc);
    Some(GlobalProposalCursor {
        last_originating_timestamp: timestamp,
        last_account_id: record.account_id.clone(),
        last_nonce: i64::try_from(record.proposal.nonce).ok()?,
        last_commitment: record.commitment.clone(),
    })
}

/// Process-now entry point for tests, demos and e2e endpoints: one full
/// walk of the fleet plus one hot pass, unpaced, releasing on the first
/// observation. Mirrors what the worker does over a rotation.
pub async fn run_release_sweep_now(state: &AppState) -> Result<SweepSummary> {
    let sweeper = ReleaseSweeper::new(
        state.clone(),
        1,
        Arc::new(SweepState::default()),
        CancellationToken::new(),
    );
    let mut summary = SweepSummary::default();
    let mut after: Option<String> = None;
    loop {
        let page = sweeper.list_page(after.as_deref(), 100).await?;
        if page.is_empty() {
            break;
        }
        for metadata in &page {
            summary.accounts += 1;
            summary.failed_accounts += usize::from(sweeper.visit_absorbing(metadata).await);
        }
        after = page.last().map(|m| m.account_id.clone());
    }
    let hot = sweeper.hot_pass().await?;
    summary.accounts += hot.accounts;
    summary.failed_accounts += hot.failed_accounts;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::kinds;
    use crate::delta_object::{DeltaObject, DeltaStatus};
    use crate::metadata::auth::Auth;
    use crate::metadata::{AccountMetadata, NetworkConfig};
    use crate::testing::helpers::{CapturingAuditor, create_test_app_state_with_mocks};
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use guardian_shared::SignatureScheme;

    const STORED: &str = "0xstored_commitment";
    const ON_CHAIN: &str = "0xchain_commitment";
    const FOREIGN: &str = "0xforeign_guardian";
    const PROPOSAL_ID: &str = "0xswitch_proposal";

    fn miden_meta(account_id: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec!["0xc1".into()],
            },
            network_config: NetworkConfig::miden_default(),
            created_at: "2026-09-01T00:00:00Z".into(),
            updated_at: "2026-09-01T00:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    fn stored_state(account_id: &str, commitment: &str) -> StateObject {
        StateObject {
            account_id: account_id.to_string(),
            commitment: commitment.to_string(),
            state_json: serde_json::json!({}),
            created_at: "2026-09-01T00:00:00Z".to_string(),
            updated_at: "2026-09-01T00:00:00Z".to_string(),
            auth_scheme: String::new(),
        }
    }

    fn visible(guardian: Option<&str>) -> OnChainGuardianBinding {
        OnChainGuardianBinding::Visible {
            on_chain_commitment: ON_CHAIN.to_string(),
            guardian_commitment: guardian.map(str::to_string),
        }
    }

    fn switch_proposal(account_id: &str, prev_commitment: &str) -> ProposalRecord {
        ProposalRecord {
            account_id: account_id.to_string(),
            commitment: PROPOSAL_ID.to_string(),
            proposal: DeltaObject {
                account_id: account_id.to_string(),
                nonce: 2,
                prev_commitment: prev_commitment.to_string(),
                // The mock storage derives a record's id from this field.
                new_commitment: Some(PROPOSAL_ID.to_string()),
                delta_payload: serde_json::json!({
                    "tx_summary": {"data": "AAAA"},
                    "signatures": [],
                    "metadata": {"proposal_type": "switch_guardian"}
                }),
                ack_sig: String::new(),
                ack_pubkey: String::new(),
                ack_scheme: String::new(),
                status: DeltaStatus::Pending {
                    timestamp: "2026-09-01T00:00:00Z".to_string(),
                    proposer_id: "0xc1".to_string(),
                    cosigner_sigs: vec![],
                },
                metadata: None,
            },
        }
    }

    struct Harness {
        sweeper: ReleaseSweeper,
        network: Arc<MockNetworkClient>,
        metadata: Arc<MockMetadataStore>,
        storage: Arc<MockStorageBackend>,
        auditor: CapturingAuditor,
    }

    /// Sweeper over the given mocks. Mock queues pop LIFO, so callers
    /// queue responses in reverse order of use.
    fn harness(
        storage: MockStorageBackend,
        network: MockNetworkClient,
        metadata: MockMetadataStore,
        confirmations: u32,
    ) -> Harness {
        let network = Arc::new(network);
        let metadata = Arc::new(metadata);
        let storage = Arc::new(storage);
        let auditor = CapturingAuditor::new();
        let mut state =
            create_test_app_state_with_mocks(storage.clone(), network.clone(), metadata.clone());
        state.auditor = Arc::new(auditor.clone());
        let sweeper = ReleaseSweeper::new(
            state,
            confirmations,
            Arc::new(SweepState::default()),
            CancellationToken::new(),
        );
        Harness {
            sweeper,
            network,
            metadata,
            storage,
            auditor,
        }
    }

    fn released_ids(metadata: &MockMetadataStore) -> Vec<String> {
        metadata.set_released_calls.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn releases_a_public_account_after_the_configured_confirmations() {
        // Visit 1: foreign key observed -> confirming. Visit 2: same key
        // -> released, audited with the chain-sweep evidence.
        let account_id = "0xacc";
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(account_id, STORED)))
                .with_pull_state(Ok(stored_state(account_id, STORED)))
                .with_pull_state(Ok(stored_state(account_id, STORED))),
            MockNetworkClient::new()
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: ON_CHAIN.into(),
                }))
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: ON_CHAIN.into(),
                }))
                .with_fetch_on_chain_guardian_binding(Ok(visible(Some(FOREIGN))))
                .with_fetch_on_chain_guardian_binding(Ok(visible(Some(FOREIGN)))),
            MockMetadataStore::new(),
            2,
        );
        let meta = miden_meta(account_id);

        h.sweeper.visit(&meta).await.expect("visit 1");
        assert!(
            released_ids(&h.metadata).is_empty(),
            "one observation is not enough"
        );
        assert_eq!(h.sweeper.sweep_state().streak(account_id), Some(1));
        assert_eq!(
            h.sweeper.sweep_state().pending_confirmations(),
            vec![account_id.to_string()],
            "the account joins the hot set until confirmed"
        );

        h.sweeper.visit(&meta).await.expect("visit 2");
        assert_eq!(released_ids(&h.metadata), vec![account_id.to_string()]);
        assert_eq!(h.sweeper.sweep_state().streak(account_id), None);
        let events = h.auditor.snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action_kind, kinds::ACCOUNTS_RELEASE);
        assert_eq!(events[0].payload["detected_by"], "chain_sweep");
        assert_eq!(events[0].payload["new_guardian_commitment"], FOREIGN);
        assert_eq!(events[0].payload["on_chain_commitment"], ON_CHAIN);
        assert_eq!(events[0].payload["stored_commitment"], STORED);
    }

    #[tokio::test]
    async fn a_pending_switch_proposal_matching_the_chain_releases_without_published_storage() {
        // A private account: the storage read would be opaque. The
        // pending switch proposal's post-state equals the on-chain
        // commitment, which proves the switch executed: released on the
        // first visit, audited with the proposal id, proposal finalized,
        // no storage read at all.
        let account_id = "0xacc";
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(account_id, STORED)))
                .with_pull_state(Ok(stored_state(account_id, STORED)))
                .with_pull_all_delta_proposals(Ok(vec![
                    switch_proposal(account_id, STORED).proposal,
                ])),
            MockNetworkClient::new()
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: ON_CHAIN.into(),
                }))
                .with_apply_delta(Ok((serde_json::json!({"post": true}), ON_CHAIN.into())))
                .with_extract_guardian_commitment(Ok(Some(FOREIGN.into())))
                .with_fetch_on_chain_guardian_binding(Ok(OnChainGuardianBinding::Opaque)),
            MockMetadataStore::new(),
            2,
        );

        h.sweeper
            .visit(&miden_meta(account_id))
            .await
            .expect("visit");
        assert_eq!(released_ids(&h.metadata), vec![account_id.to_string()]);
        assert!(
            h.network
                .get_fetch_on_chain_guardian_binding_calls()
                .is_empty(),
            "a proposal match needs no storage read"
        );
        let events = h.auditor.snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["detected_by"], "proposal_match");
        assert_eq!(events[0].payload["proposal_id"], PROPOSAL_ID);
        assert_eq!(events[0].payload["new_guardian_commitment"], FOREIGN);
        assert_eq!(events[0].payload["on_chain_commitment"], ON_CHAIN);
        assert_eq!(events[0].payload["stored_commitment"], STORED);
        assert_eq!(
            h.storage.get_delete_delta_proposal_calls(),
            vec![(account_id.to_string(), PROPOSAL_ID.to_string())],
            "the executed switch proposal is finalized"
        );
    }

    #[tokio::test]
    async fn a_switch_proposal_whose_post_state_is_not_the_chain_head_falls_back_to_storage() {
        // The chain moved somewhere else (busy account under the new
        // guardian, or an unrelated landing): no proposal match, and the
        // private account stays opaque. The post-state is cached so the
        // reconstruction runs once.
        let account_id = "0xacc";
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(account_id, STORED)))
                .with_pull_state(Ok(stored_state(account_id, STORED)))
                .with_pull_all_delta_proposals(Ok(vec![
                    switch_proposal(account_id, STORED).proposal,
                ]))
                .with_pull_all_delta_proposals(Ok(vec![
                    switch_proposal(account_id, STORED).proposal,
                ])),
            MockNetworkClient::new()
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: "0xsomewhere_else".into(),
                }))
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: "0xsomewhere_else".into(),
                }))
                .with_apply_delta(Ok((serde_json::json!({}), ON_CHAIN.into())))
                .with_extract_guardian_commitment(Ok(Some(FOREIGN.into()))),
            MockMetadataStore::new(),
            1,
        );
        let meta = miden_meta(account_id);

        h.sweeper.visit(&meta).await.expect("visit 1");
        h.sweeper.visit(&meta).await.expect("visit 2");
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(
            h.network.apply_delta_responses.lock().unwrap().len(),
            0,
            "the single queued reconstruction was consumed once and cached"
        );
        assert_eq!(
            h.network.get_fetch_on_chain_guardian_binding_calls().len(),
            2,
            "without a match the storage read is the fallback"
        );
    }

    #[tokio::test]
    async fn hot_targets_include_streaks_and_accounts_with_pending_switch_proposals() {
        let h = harness(
            MockStorageBackend::new().with_list_global_proposals_paged(Ok(vec![
                switch_proposal("0xswitching", STORED),
                {
                    let mut other = switch_proposal("0xpaying", STORED);
                    other.proposal.delta_payload["metadata"]["proposal_type"] = "p2id".into();
                    other
                },
            ])),
            MockNetworkClient::new(),
            MockMetadataStore::new()
                .with_get(Ok(Some(miden_meta("0xswitching"))))
                .with_get(Ok(Some(miden_meta("0xconfirming")))),
            2,
        );
        h.sweeper
            .sweep_state()
            .record_foreign_observation("0xconfirming", FOREIGN);

        let targets = h.sweeper.hot_targets().await.expect("hot targets");
        let ids: Vec<&str> = targets.iter().map(|m| m.account_id.as_str()).collect();
        assert_eq!(ids, vec!["0xconfirming", "0xswitching"]);
    }

    #[tokio::test]
    async fn a_still_bound_lagging_account_is_never_released() {
        let account_id = "0xacc";
        let h = harness(
            MockStorageBackend::new().with_pull_state(Ok(stored_state(account_id, STORED))),
            MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: ON_CHAIN.into(),
            })),
            MockMetadataStore::new(),
            1,
        );
        let own = h.sweeper.state.ack.commitment(&SignatureScheme::Falcon);
        h.network
            .fetch_on_chain_guardian_binding_responses
            .lock()
            .unwrap()
            .push(Ok(visible(Some(&own))));

        h.sweeper
            .visit(&miden_meta(account_id))
            .await
            .expect("visit");
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(h.sweeper.sweep_state().streak(account_id), None);
        assert!(h.auditor.snapshot().is_empty());
    }

    #[tokio::test]
    async fn opaque_storage_and_missing_binding_never_release() {
        for binding in [OnChainGuardianBinding::Opaque, visible(None)] {
            let account_id = "0xacc";
            let h = harness(
                MockStorageBackend::new().with_pull_state(Ok(stored_state(account_id, STORED))),
                MockNetworkClient::new()
                    .with_verify_commitment(Ok(StateVerification::Mismatch {
                        on_chain: ON_CHAIN.into(),
                    }))
                    .with_fetch_on_chain_guardian_binding(Ok(binding.clone())),
                MockMetadataStore::new(),
                1,
            );
            h.sweeper
                .visit(&miden_meta(account_id))
                .await
                .expect("visit");
            assert!(
                released_ids(&h.metadata).is_empty(),
                "{binding:?} must never release"
            );
            assert_eq!(h.sweeper.sweep_state().streak(account_id), None);
        }
    }

    #[tokio::test]
    async fn chain_at_the_stored_base_skips_every_detector_and_clears_the_streak() {
        for verification in [StateVerification::Match, StateVerification::Absent] {
            let account_id = "0xacc";
            let h = harness(
                MockStorageBackend::new()
                    .with_pull_state(Ok(stored_state(account_id, STORED)))
                    .with_pull_all_delta_proposals(Ok(vec![
                        switch_proposal(account_id, STORED).proposal,
                    ])),
                MockNetworkClient::new()
                    .with_verify_commitment(Ok(verification.clone()))
                    .with_fetch_on_chain_guardian_binding(Ok(visible(Some(FOREIGN)))),
                MockMetadataStore::new(),
                1,
            );
            h.sweeper
                .sweep_state()
                .record_foreign_observation(account_id, FOREIGN);

            h.sweeper
                .visit(&miden_meta(account_id))
                .await
                .expect("visit");
            assert!(
                h.network
                    .get_fetch_on_chain_guardian_binding_calls()
                    .is_empty()
            );
            assert_eq!(h.network.apply_delta_responses.lock().unwrap().len(), 0);
            assert!(released_ids(&h.metadata).is_empty());
            assert_eq!(
                h.sweeper.sweep_state().streak(account_id),
                None,
                "{verification:?} clears a stale streak"
            );
        }
    }

    #[tokio::test]
    async fn probe_failures_defer_without_touching_the_streak() {
        let account_id = "0xacc";
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(account_id, STORED)))
                .with_pull_state(Ok(stored_state(account_id, STORED))),
            MockNetworkClient::new()
                // visit 2: the storage read fails
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: ON_CHAIN.into(),
                }))
                .with_fetch_on_chain_guardian_binding(Err("rpc down".into()))
                // visit 1: the commitment probe fails
                .with_verify_commitment(Err("rpc down".into())),
            MockMetadataStore::new(),
            2,
        );
        h.sweeper
            .sweep_state()
            .record_foreign_observation(account_id, FOREIGN);
        let meta = miden_meta(account_id);

        assert!(
            !h.sweeper.visit_absorbing(&meta).await,
            "a probe failure is a deferral"
        );
        assert_eq!(h.sweeper.sweep_state().streak(account_id), Some(1));
        assert!(!h.sweeper.visit_absorbing(&meta).await);
        assert_eq!(h.sweeper.sweep_state().streak(account_id), Some(1));
        assert!(released_ids(&h.metadata).is_empty());
    }

    #[tokio::test]
    async fn pending_candidate_evm_and_released_rows_are_not_probed() {
        let mut pending = miden_meta("0xpending");
        pending.has_pending_candidate = true;
        let mut evm = miden_meta("evm:1:0xabc");
        evm.network_config = NetworkConfig::Evm {
            chain_id: 1,
            account_address: "0xabc".into(),
            multisig_validator_address: "0xdef".into(),
        };
        let mut released = miden_meta("0xreleased");
        released.released_at = Some(chrono::Utc::now());
        let h = harness(
            MockStorageBackend::new(),
            MockNetworkClient::new(),
            MockMetadataStore::new(),
            1,
        );
        for meta in [pending, evm, released] {
            h.sweeper.visit(&meta).await.expect("visit");
        }
        assert!(h.network.get_verify_commitment_calls().is_empty());
        assert!(released_ids(&h.metadata).is_empty());
    }

    #[tokio::test]
    async fn a_stored_base_that_moved_before_the_write_defers_the_release() {
        let account_id = "0xacc";
        let h = harness(
            MockStorageBackend::new()
                // re-read right before the write (popped second)
                .with_pull_state(Ok(stored_state(account_id, "0xreconfigured")))
                // initial read (popped first)
                .with_pull_state(Ok(stored_state(account_id, STORED))),
            MockNetworkClient::new()
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: ON_CHAIN.into(),
                }))
                .with_fetch_on_chain_guardian_binding(Ok(visible(Some(FOREIGN)))),
            MockMetadataStore::new(),
            1,
        );
        h.sweeper
            .visit(&miden_meta(account_id))
            .await
            .expect("visit");
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(h.sweeper.sweep_state().streak(account_id), None);
        assert!(h.auditor.snapshot().is_empty());
    }

    #[tokio::test]
    async fn disagreeing_reads_are_not_an_observation() {
        let account_id = "0xacc";
        let h = harness(
            MockStorageBackend::new().with_pull_state(Ok(stored_state(account_id, STORED))),
            MockNetworkClient::new()
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: ON_CHAIN.into(),
                }))
                .with_fetch_on_chain_guardian_binding(Ok(OnChainGuardianBinding::Visible {
                    on_chain_commitment: STORED.into(),
                    guardian_commitment: Some(FOREIGN.into()),
                })),
            MockMetadataStore::new(),
            1,
        );
        h.sweeper
            .visit(&miden_meta(account_id))
            .await
            .expect("visit");
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(h.sweeper.sweep_state().streak(account_id), None);
    }

    #[tokio::test]
    async fn a_failed_release_write_counts_the_account_as_failed_and_keeps_the_streak() {
        let account_id = "0xacc";
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(account_id, STORED)))
                .with_pull_state(Ok(stored_state(account_id, STORED))),
            MockNetworkClient::new()
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: ON_CHAIN.into(),
                }))
                .with_fetch_on_chain_guardian_binding(Ok(visible(Some(FOREIGN)))),
            MockMetadataStore::new().with_set_released(Err("db down".into())),
            1,
        );
        assert!(h.sweeper.visit_absorbing(&miden_meta(account_id)).await);
        assert_eq!(h.sweeper.sweep_state().streak(account_id), Some(1));
        assert!(h.auditor.snapshot().is_empty());
    }

    #[tokio::test]
    async fn process_now_walks_every_page_and_the_hot_set() {
        let h = harness(
            MockStorageBackend::new(),
            MockNetworkClient::new(),
            MockMetadataStore::new()
                // page 3: empty (popped last)
                .with_list_release_sweep_page(Ok(vec![]))
                // page 2
                .with_list_release_sweep_page(Ok(vec![miden_meta("0xb")]))
                // page 1
                .with_list_release_sweep_page(Ok(vec![miden_meta("0xa")])),
            1,
        );
        // Both accounts lack stored state in the mock: counted as failed
        // visits, but the walk completes.
        let summary = run_release_sweep_now(&h.sweeper.state).await.expect("walk");
        assert_eq!(summary.accounts, 2);
        assert_eq!(summary.failed_accounts, 2);
        assert_eq!(
            h.metadata.get_list_release_sweep_page_calls(),
            vec![
                (None, 100),
                (Some("0xa".into()), 100),
                (Some("0xb".into()), 100)
            ]
        );
    }

    #[test]
    fn foreign_observation_streak_counts_agreeing_keys_only() {
        let state = SweepState::default();
        assert_eq!(state.record_foreign_observation("a", "0xb"), 1);
        assert_eq!(state.record_foreign_observation("a", "0xb"), 2);
        assert_eq!(state.record_foreign_observation("a", "0xc"), 1);
        assert_eq!(state.pending_confirmations(), vec!["a".to_string()]);
        state.clear_streak("a");
        assert!(state.pending_confirmations().is_empty());
    }
}
