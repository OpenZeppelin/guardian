//! Per-account release detection: the probes, the detectors, and the
//! cross-visit state (confirmation streaks and their re-check queue,
//! cached switch evidence, stored states already verified). The worker in
//! `worker.rs` decides *when* an account is visited; everything here
//! decides *what* a visit does.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;

use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, Result};
use crate::metadata::AccountMetadata;
use crate::metrics::labels::ReleaseSweepOutcome;
use crate::network::{OnChainGuardianBinding, RpcReadMode, StateVerification};
use crate::services::release_on_switch::{
    ReleaseEvidence, ReleaseWrite, own_guardian_commitment, release_switched_account,
};
use crate::state::AppState;
use crate::state_object::StateObject;

/// How long client-abandoned deltas stay in scope when the server runs
/// without canonicalization settings (retained rows are always in scope).
const DEFAULT_ABANDONED_WINDOW_SECONDS: u64 = 86_400;

/// What one walk (a rotation or a process-now run) did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepSummary {
    pub accounts: usize,
    /// Accounts that could not be checked: a failed visit, or a chain
    /// read that failed and was deferred to a later visit.
    pub failed_accounts: usize,
}

/// What one visit amounted to, for the walk's own accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitOutcome {
    /// The account was checked, whatever it showed.
    Checked,
    /// A chain read failed; a later visit checks the account again.
    Deferred,
    /// The visit failed (storage error, failed release write).
    Failed,
}

/// A candidate post-state's origin: a pending proposal or an unpromoted
/// delta, both chaining from the stored base.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SwitchSource {
    /// `storage_id` as the storage backend keys it (the filesystem
    /// backend keeps proposal ids unprefixed); `proposal_id` as the API
    /// and the audit trail spell it.
    Proposal {
        storage_id: String,
        proposal_id: String,
    },
    /// A retained or client-abandoned delta.
    Delta { nonce: u64 },
}

impl SwitchSource {
    /// Cache key, unique per account.
    fn key(&self) -> String {
        match self {
            Self::Proposal { storage_id, .. } => format!("proposal:{storage_id}"),
            Self::Delta { nonce } => format!("delta:{nonce}"),
        }
    }
}

/// A candidate's post-state, computed once from the stored base.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SwitchPostState {
    Ready {
        post_commitment: String,
        guardian_commitment: Option<String>,
    },
    /// The summary does not apply to the stored base, or its post-state
    /// cannot be inspected. Cached like a success, so the reconstruction
    /// never reruns for the same candidate and base.
    Unusable,
}

/// Everything the sweep knows about one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SwitchEvidence {
    /// The stored base the post-state was computed from; an entry for
    /// another base is stale.
    base_commitment: String,
    post_state: SwitchPostState,
    /// First block not yet searched for a transaction ending at the
    /// post-state (blocks are final, so a searched block never has to be
    /// read again).
    resume_from_block: u32,
}

/// An open confirmation streak: a foreign guardian key seen in the
/// account's published storage.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Streak {
    guardian_commitment: String,
    observations: u32,
    last_block: u32,
}

/// What recording one storage observation did to the streak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Observation {
    /// A new observation; the streak now holds this many.
    Counted(u32),
    /// The same key at a block no later than the last counted one: the
    /// node has not moved on, so the read confirms nothing. The streak
    /// keeps this many.
    NotNewer(u32),
}

/// Replica-local cross-visit state. A failover restarts the streaks and
/// the searches, which only delays a release.
#[derive(Default)]
pub struct SweepState {
    /// account_id → open confirmation streak.
    streaks: Mutex<BTreeMap<String, Streak>>,
    /// Accounts due for a confirmation re-check, in due order, at most
    /// once each.
    rechecks: Mutex<VecDeque<(Instant, String)>>,
    /// account_id → candidate key → evidence. Only accounts off their
    /// stored base with candidates have entries, and only for candidates
    /// that still exist.
    switch_evidence: Mutex<HashMap<String, HashMap<String, SwitchEvidence>>>,
    /// account_id → hash of the stored commitment whose guardian key was
    /// checked while the chain held it (this server's, absent, or not
    /// attributable to a switch), so a healthy account's state is parsed
    /// once per stored state.
    checked_stored: Mutex<HashMap<String, u64>>,
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn commitment_hash(commitment: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    commitment.hash(&mut hasher);
    hasher.finish()
}

impl SweepState {
    /// Observations in the account's open streak, if any.
    pub fn streak(&self, account_id: &str) -> Option<u32> {
        locked(&self.streaks)
            .get(account_id)
            .map(|streak| streak.observations)
    }

    /// Record one observation of `guardian_commitment` (a key that is not
    /// this server's) read at `block_num`. A different key than the one
    /// on record restarts the streak: confirmations must agree on *which*
    /// key the chain shows. The same key only counts again from a
    /// strictly later block.
    fn record_foreign_observation(
        &self,
        account_id: &str,
        guardian_commitment: &str,
        block_num: u32,
    ) -> Observation {
        let mut streaks = locked(&self.streaks);
        match streaks.get_mut(account_id) {
            Some(streak) if streak.guardian_commitment == guardian_commitment => {
                if block_num > streak.last_block {
                    streak.observations = streak.observations.saturating_add(1);
                    streak.last_block = block_num;
                    Observation::Counted(streak.observations)
                } else {
                    Observation::NotNewer(streak.observations)
                }
            }
            _ => {
                streaks.insert(
                    account_id.to_string(),
                    Streak {
                        guardian_commitment: guardian_commitment.to_string(),
                        observations: 1,
                        last_block: block_num,
                    },
                );
                Observation::Counted(1)
            }
        }
    }

    /// Close the account's streak and drop its queued re-check.
    fn clear_streak(&self, account_id: &str) {
        locked(&self.streaks).remove(account_id);
        locked(&self.rechecks).retain(|(_, queued)| queued != account_id);
    }

    /// Queue a re-check of `account_id` at `due`, unless one is queued.
    fn schedule_recheck(&self, account_id: &str, due: Instant) {
        let mut rechecks = locked(&self.rechecks);
        if rechecks.iter().any(|(_, queued)| queued == account_id) {
            return;
        }
        let position = rechecks.partition_point(|(at, _)| *at <= due);
        rechecks.insert(position, (due, account_id.to_string()));
    }

    /// When the earliest queued re-check is due.
    pub fn next_recheck_due(&self) -> Option<Instant> {
        locked(&self.rechecks).front().map(|(due, _)| *due)
    }

    /// Take the earliest re-check if it is due at `now`.
    pub fn pop_due_recheck(&self, now: Instant) -> Option<String> {
        let mut rechecks = locked(&self.rechecks);
        match rechecks.front() {
            Some((due, _)) if *due <= now => rechecks.pop_front().map(|(_, account)| account),
            _ => None,
        }
    }

    /// Accounts with a queued re-check, in due order.
    #[cfg(test)]
    pub fn pending_rechecks(&self) -> Vec<String> {
        locked(&self.rechecks)
            .iter()
            .map(|(_, account)| account.clone())
            .collect()
    }

    /// Cached evidence for the candidate `key`, if computed from `base`.
    fn switch_evidence(&self, account_id: &str, key: &str, base: &str) -> Option<SwitchEvidence> {
        locked(&self.switch_evidence)
            .get(account_id)
            .and_then(|candidates| candidates.get(key))
            .filter(|evidence| evidence.base_commitment == base)
            .cloned()
    }

    fn remember_switch(&self, account_id: &str, key: &str, evidence: SwitchEvidence) {
        locked(&self.switch_evidence)
            .entry(account_id.to_string())
            .or_default()
            .insert(key.to_string(), evidence);
    }

    fn set_resume_block(&self, account_id: &str, key: &str, block: u32) {
        if let Some(evidence) = locked(&self.switch_evidence)
            .get_mut(account_id)
            .and_then(|candidates| candidates.get_mut(key))
        {
            evidence.resume_from_block = block;
        }
    }

    /// Keep only the evidence of candidates in `current`: a proposal
    /// deleted by canonicalization or by its users, a delta that aged
    /// out, or anything that no longer chains from the stored base leaves
    /// the cache.
    fn retain_switches(&self, account_id: &str, current: &HashSet<String>) {
        let mut cache = locked(&self.switch_evidence);
        if let Some(candidates) = cache.get_mut(account_id) {
            candidates.retain(|key, _| current.contains(key));
            if candidates.is_empty() {
                cache.remove(account_id);
            }
        }
    }

    /// Number of candidates with cached evidence for `account_id`.
    #[cfg(test)]
    pub fn cached_switches(&self, account_id: &str) -> usize {
        locked(&self.switch_evidence)
            .get(account_id)
            .map_or(0, HashMap::len)
    }

    fn is_stored_checked(&self, account_id: &str, commitment: &str) -> bool {
        locked(&self.checked_stored).get(account_id) == Some(&commitment_hash(commitment))
    }

    fn mark_stored_checked(&self, account_id: &str, commitment: &str) {
        locked(&self.checked_stored).insert(account_id.to_string(), commitment_hash(commitment));
    }

    /// Forget that the account's stored state was checked.
    #[cfg(test)]
    fn forget_stored_check(&self, account_id: &str) {
        locked(&self.checked_stored).remove(account_id);
    }

    /// Drop everything held for `account_id`: it was released, re-based
    /// or is no longer sweepable.
    fn forget_account(&self, account_id: &str) {
        self.clear_streak(account_id);
        locked(&self.switch_evidence).remove(account_id);
        locked(&self.checked_stored).remove(account_id);
    }
}

fn record_outcome(outcome: ReleaseSweepOutcome) {
    metrics::counter!(
        crate::metrics::names::RELEASE_SWEEP_ACCOUNTS_TOTAL,
        crate::metrics::names::LABEL_OUTCOME => outcome.as_str()
    )
    .increment(1);
}

/// A candidate whose post-state the chain reached.
struct ExecutedSwitch {
    source: SwitchSource,
    guardian_commitment: Option<String>,
    switch_commitment: String,
    /// The block of the transaction that ended at the post-state; `None`
    /// when the chain sits at the post-state now.
    switch_block_num: Option<u32>,
}

/// Outcome of looking for an executed switch.
enum SwitchSearch {
    Found(ExecutedSwitch),
    NotFound,
    /// The transaction history could not be searched; the storage read
    /// still runs.
    HistoryUnavailable,
}

/// How a guardian key, read from chain or computed for a candidate's
/// post-state, relates to the account.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyVerdict {
    /// No guardian binding at all: absence is not a switch.
    NoBinding,
    /// This server's key: the account is still bound here.
    Own,
    /// The stored base's key, which is not this server's current one:
    /// the server's ack key changed since the account was onboarded (a
    /// new ack secret, or ephemeral keys after a restart). Nothing moved
    /// the account, so this is never a switch.
    Base,
    /// Neither: the account moved to another guardian.
    Foreign(String),
}

/// The stored base's guardian key, parsed at most once per visit and only
/// once a key other than this server's turns up (normally the base holds
/// this server's key, so the comparison changes nothing).
#[derive(Default)]
struct BaseGuardian(Option<Option<String>>);

/// What checking the stored state's own guardian key decided.
enum StoredState {
    /// The stored state proved a switch and the release was written (or
    /// a concurrent writer settled the account first).
    Released,
    /// Nothing to release; carries the stored key when it was parsed.
    Kept(BaseGuardian),
}

/// Proposal ids are `0x`-prefixed on the wire; the filesystem backend
/// keys them without the prefix.
fn canonical_proposal_id(storage_id: &str) -> String {
    crate::utils::normalize_commitment_hex(storage_id)
        .unwrap_or_else(|_| format!("0x{}", storage_id.trim_start_matches("0x")))
}

/// The release detector over one server's accounts. Built once per
/// worker around the shared [`SweepState`].
pub struct ReleaseSweeper {
    state: AppState,
    confirmations: u32,
    recheck: Duration,
    sweep: Arc<SweepState>,
}

impl ReleaseSweeper {
    pub fn new(
        state: AppState,
        confirmations: u32,
        recheck: Duration,
        sweep: Arc<SweepState>,
    ) -> Self {
        Self {
            state,
            confirmations,
            recheck,
            sweep,
        }
    }

    pub fn sweep_state(&self) -> &Arc<SweepState> {
        &self.sweep
    }

    /// Next page of account ids for the rotation walk.
    pub async fn list_page(&self, after: Option<&str>, limit: u32) -> Result<Vec<String>> {
        self.state
            .metadata
            .list_release_sweep_ids(after, limit)
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to list sweepable accounts: {e}"))
            })
    }

    /// Number of accounts one rotation walks, for pacing it.
    pub async fn fleet_size(&self) -> Result<usize> {
        self.state
            .metadata
            .count_release_sweep_accounts()
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to count sweepable accounts: {e}"))
            })
    }

    /// Visit one account, turning a failure into [`VisitOutcome::Failed`]
    /// so a walk can keep going and count it.
    pub async fn visit_absorbing(&self, account_id: &str) -> VisitOutcome {
        match self.visit_account(account_id).await {
            Ok(outcome) => outcome,
            Err(e) => {
                tracing::error!(
                    account_id = %account_id,
                    error = %e,
                    "Release sweep failed for account"
                );
                VisitOutcome::Failed
            }
        }
    }

    /// Visit one account by id. The metadata row is read here, not taken
    /// from the rotation's page: a page can be hours old by the time its
    /// last account comes up, and a re-check has only the id. An account
    /// whose confirmation streak is still open afterwards gets a re-check.
    pub async fn visit_account(&self, account_id: &str) -> Result<VisitOutcome> {
        let outcome = match self.state.metadata.get(account_id).await {
            Ok(Some(metadata)) => self.visit(&metadata).await,
            Ok(None) => {
                self.sweep.forget_account(account_id);
                Ok(VisitOutcome::Checked)
            }
            Err(e) => Err(GuardianError::StorageError(format!(
                "Failed to read account metadata: {e}"
            ))),
        };
        if self.sweep.streak(account_id).is_some() {
            self.sweep
                .schedule_recheck(account_id, Instant::now() + self.recheck);
        }
        outcome
    }

    /// Sweep one account: the cheap commitment probe, then the detectors
    /// only when the chain moved past the stored base.
    async fn visit(&self, metadata: &AccountMetadata) -> Result<VisitOutcome> {
        let account_id = metadata.account_id.as_str();

        // The store filters these out of the rotation; the checks stay for
        // re-checks and for rows that changed since they were listed. EVM
        // accounts have no on-chain guardian binding; a released row has
        // nothing left to decide.
        if metadata.network_config.is_evm() || metadata.released_at.is_some() {
            self.sweep.forget_account(account_id);
            return Ok(VisitOutcome::Checked);
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
            return Ok(VisitOutcome::Checked);
        }

        let stored_commitment = self
            .state
            .storage
            .pull_state_commitment(account_id)
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to get the stored commitment: {e}"))
            })?;

        let verification = self
            .state
            .network_client
            .verify_commitment(account_id, &stored_commitment, RpcReadMode::SingleAttempt)
            .await;
        match verification {
            // The chain holds exactly the stored state, so the stored
            // guardian key is the on-chain one: this server's (as
            // `/configure` validated) unless a promoted delta switched it
            // or this server's own key changed.
            Ok(StateVerification::Match) => {
                self.sweep.clear_streak(account_id);
                if !self.sweep.is_stored_checked(account_id, &stored_commitment) {
                    let stored = self.pull_full_state(account_id).await?;
                    if stored.commitment != stored_commitment {
                        return Ok(VisitOutcome::Checked);
                    }
                    if let StoredState::Released =
                        self.check_stored_state(metadata, &stored, true).await?
                    {
                        return Ok(VisitOutcome::Checked);
                    }
                }
                tracing::debug!(
                    event = "release_sweep_deferred",
                    reason = "chain_at_stored_base",
                    account_id = %account_id,
                    "Chain at the stored base; guardian binding unchanged"
                );
                Ok(VisitOutcome::Checked)
            }
            // No state on chain yet: nothing can have switched.
            Ok(StateVerification::Absent) => {
                self.sweep.forget_account(account_id);
                tracing::debug!(
                    event = "release_sweep_deferred",
                    reason = "not_on_chain",
                    account_id = %account_id,
                    "Account not on chain yet; guardian binding unchanged"
                );
                Ok(VisitOutcome::Checked)
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
                Ok(VisitOutcome::Deferred)
            }
            Ok(StateVerification::Mismatch { on_chain }) => {
                let stored = self.pull_full_state(account_id).await?;
                if stored.commitment != stored_commitment {
                    // The stored state moved between the two reads; the
                    // probe compared the old one. The next visit decides.
                    return Ok(VisitOutcome::Checked);
                }
                // A promoted switch whose release was never written is
                // proved by the stored state itself, however far the
                // account has moved on since.
                let base = match self.check_stored_state(metadata, &stored, false).await? {
                    StoredState::Released => return Ok(VisitOutcome::Checked),
                    StoredState::Kept(base) => base,
                };
                self.visit_diverged(metadata, &stored, &on_chain, base)
                    .await
            }
        }
    }

    async fn pull_full_state(&self, account_id: &str) -> Result<StateObject> {
        self.state
            .storage
            .pull_state(account_id)
            .await
            .map_err(|e| GuardianError::StorageError(format!("Failed to get current state: {e}")))
    }

    /// Whether the stored state itself proves a switch: its guardian key
    /// is not this server's, and the canonical delta that produced it
    /// carries an ack signed with this server's current key. That is a
    /// promoted switch whose push-path release was never written (the
    /// hook's write failed, or a server predating the hook promoted it),
    /// released here with the evidence the push path would have recorded,
    /// wherever the chain has gone since. A foreign stored key nothing
    /// explains means this server's own ack key changed: reported when
    /// `report_mismatch` (the chain holds the stored state, so no other
    /// detector will), never released. Parsed once per stored commitment.
    async fn check_stored_state(
        &self,
        metadata: &AccountMetadata,
        stored: &StateObject,
        report_mismatch: bool,
    ) -> Result<StoredState> {
        let account_id = metadata.account_id.as_str();
        if self.sweep.is_stored_checked(account_id, &stored.commitment) {
            return Ok(StoredState::Kept(BaseGuardian::default()));
        }
        let own_commitment = own_guardian_commitment(&self.state, metadata);
        let key = self.guardian_key_of(&stored.state_json).await?;
        let foreign = key
            .as_deref()
            .filter(|guardian| *guardian != own_commitment);
        if let Some(guardian) = foreign {
            if let Some(delta_nonce) = self.promoting_delta_nonce(metadata, stored).await? {
                tracing::info!(
                    event = "release_sweep_unwritten_push_release",
                    account_id = %account_id,
                    delta_nonce,
                    stored_commitment = %stored.commitment,
                    new_guardian_commitment = %guardian,
                    "The stored state is a promoted delta's whose guardian key is not this \
                     server's, and its push-path release was never written"
                );
                self.release(
                    metadata,
                    guardian,
                    ReleaseEvidence::Delta {
                        delta_nonce,
                        new_commitment: &stored.commitment,
                    },
                )
                .await?;
                return Ok(StoredState::Released);
            }
            if report_mismatch {
                self.report_own_key_mismatch(account_id, guardian, &own_commitment);
            }
        }
        // An unexplained foreign key off the chain's state stays unchecked,
        // so it is reported once the chain holds the stored state.
        if foreign.is_none() || report_mismatch {
            self.sweep
                .mark_stored_checked(account_id, &stored.commitment);
        }
        Ok(StoredState::Kept(BaseGuardian(Some(key))))
    }

    /// The nonce of the canonical delta that produced the stored state,
    /// when its ack was signed with this server's current key: proof that
    /// the delta, not a change of this server's own key, moved the
    /// guardian key away. The signature itself is checked (every storage
    /// backend keeps it), off the executor.
    async fn promoting_delta_nonce(
        &self,
        metadata: &AccountMetadata,
        stored: &StateObject,
    ) -> Result<Option<u64>> {
        let latest = self
            .state
            .storage
            .list_canonical_deltas_paged(&metadata.account_id, 1, None)
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!(
                    "Failed to read the latest canonical delta: {e}"
                ))
            })?;
        let Some(delta) = latest.into_iter().next().filter(|delta| {
            delta
                .new_commitment
                .as_deref()
                .is_some_and(|commitment| commitment.eq_ignore_ascii_case(&stored.commitment))
        }) else {
            return Ok(None);
        };
        let nonce = delta.nonce;
        let ack = self.state.ack.clone();
        let scheme = metadata.auth.scheme();
        let acked = crate::network::reconstructor()
            .run_background(move || Ok::<_, String>(ack.acked_with_current_key(&delta, &scheme)))
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!(
                    "Failed to check the latest delta's ack: {}",
                    GuardianError::from(e)
                ))
            })?;
        Ok(acked.then_some(nonce))
    }

    /// Classify a guardian key against this server's and the stored
    /// base's. A key is foreign only when it is neither: comparing with
    /// the base too keeps a server whose own ack key changed from reading
    /// every account it onboarded under the previous key as switched.
    async fn classify_key(
        &self,
        key: Option<&str>,
        own_commitment: &str,
        stored: &StateObject,
        base: &mut BaseGuardian,
    ) -> Result<KeyVerdict> {
        let Some(key) = key else {
            return Ok(KeyVerdict::NoBinding);
        };
        if key == own_commitment {
            return Ok(KeyVerdict::Own);
        }
        if base.0.is_none() {
            base.0 = Some(self.guardian_key_of(&stored.state_json).await?);
        }
        Ok(if base.0.as_ref().and_then(Option::as_deref) == Some(key) {
            KeyVerdict::Base
        } else {
            KeyVerdict::Foreign(key.to_string())
        })
    }

    /// The account is bound to a key this server does not hold, and
    /// nothing this server can see moved it there: the server's ack key
    /// changed. Nothing is released; restoring the key is the fix.
    fn report_own_key_mismatch(&self, account_id: &str, guardian: &str, own_commitment: &str) {
        tracing::warn!(
            event = "release_sweep_own_key_mismatch",
            account_id = %account_id,
            guardian_commitment = %guardian,
            own_guardian_commitment = %own_commitment,
            "Account is bound to a guardian key this server does not hold, and no switch \
             explains it: this server's ack key changed since the account was onboarded. \
             Not released"
        );
        record_outcome(ReleaseSweepOutcome::OwnKeyMismatch);
    }

    /// The guardian key of an account state, parsed off the executor (a
    /// state can be large).
    async fn guardian_key_of(&self, state_json: &serde_json::Value) -> Result<Option<String>> {
        let client = self.state.network_client.clone();
        let state_json = state_json.clone();
        crate::network::reconstructor()
            .run_background(move || client.extract_guardian_commitment(&state_json))
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!(
                    "Failed to read the guardian key of a stored state: {}",
                    GuardianError::from(e)
                ))
            })
    }

    /// The nonce of an account state, parsed off the executor.
    async fn nonce_of(&self, state_json: &serde_json::Value) -> Result<Option<u64>> {
        let client = self.state.network_client.clone();
        let state_json = state_json.clone();
        crate::network::reconstructor()
            .run_background(move || client.account_nonce(&state_json))
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!(
                    "Failed to read the stored state's nonce: {}",
                    GuardianError::from(e)
                ))
            })
    }

    /// The chain moved past the stored base. Either a transaction this
    /// server acknowledged landed without the store catching up (the
    /// guardian key is still ours — issue #345 territory, not a switch),
    /// or the one transaction the account can execute without this
    /// server's signature ran: a guardian key rotation. A pending proposal
    /// or unpromoted delta whose post-state the chain reached proves the
    /// latter for any account; published storage tells the two apart for
    /// public accounts.
    async fn visit_diverged(
        &self,
        metadata: &AccountMetadata,
        stored: &StateObject,
        on_chain: &str,
        mut base: BaseGuardian,
    ) -> Result<VisitOutcome> {
        let account_id = metadata.account_id.as_str();
        let own_commitment = own_guardian_commitment(&self.state, metadata);

        // Detector 1: a candidate whose post-state the chain reached — now,
        // or at any block of the account's history. An exact commitment
        // is proof (a lagging node cannot invent it), so it needs no
        // confirmation.
        let history_unavailable = match self
            .find_executed_switch(account_id, stored, on_chain, &own_commitment, &mut base)
            .await?
        {
            SwitchSearch::Found(executed) => {
                return self
                    .release_on_switch_match(
                        metadata,
                        stored,
                        on_chain,
                        &own_commitment,
                        executed,
                        &mut base,
                    )
                    .await;
            }
            SwitchSearch::NotFound => false,
            SwitchSearch::HistoryUnavailable => true,
        };

        // Detector 2: published storage.
        let binding = self
            .state
            .network_client
            .fetch_on_chain_guardian_binding(account_id, RpcReadMode::SingleAttempt)
            .await;
        let (on_chain_commitment, guardian_commitment, on_chain_nonce, block_num) = match binding {
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
                return Ok(VisitOutcome::Deferred);
            }
            // A private account whose only evidence is in the history this
            // visit could not read: retry rather than report it opaque.
            Ok(OnChainGuardianBinding::Opaque) if history_unavailable => {
                record_outcome(ReleaseSweepOutcome::ProbeFailed);
                return Ok(VisitOutcome::Deferred);
            }
            Ok(OnChainGuardianBinding::Opaque) => {
                tracing::info!(
                    event = "release_sweep_deferred",
                    reason = "storage_opaque",
                    account_id = %account_id,
                    stored_commitment = %stored.commitment,
                    on_chain = %on_chain,
                    "Chain state differs from the stored one, no pending proposal or \
                     unpromoted delta explains it, and the account does not publish its \
                     storage; the guardian binding cannot be verified from chain"
                );
                record_outcome(ReleaseSweepOutcome::StorageOpaque);
                return Ok(VisitOutcome::Checked);
            }
            Ok(OnChainGuardianBinding::Visible {
                on_chain_commitment,
                guardian_commitment,
                nonce,
                block_num,
            }) => (on_chain_commitment, guardian_commitment, nonce, block_num),
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
            return Ok(VisitOutcome::Checked);
        }

        // A read of a state older than the stored one is not the chain
        // moving on: the stored state has not landed yet (for example just
        // re-onboarded after a switch to this server) or the node lags. It
        // is never evidence, however far behind the node is.
        if let Some(stored_nonce) = self.nonce_of(&stored.state_json).await?
            && on_chain_nonce < stored_nonce
        {
            tracing::info!(
                event = "release_sweep_deferred",
                reason = "chain_behind_stored",
                account_id = %account_id,
                on_chain_nonce,
                stored_nonce,
                block_num,
                "Storage read observed a state older than the stored one; ignoring it"
            );
            record_outcome(ReleaseSweepOutcome::ChainBehindStored);
            return Ok(VisitOutcome::Checked);
        }

        let new_guardian_commitment = match self
            .classify_key(
                guardian_commitment.as_deref(),
                &own_commitment,
                stored,
                &mut base,
            )
            .await?
        {
            KeyVerdict::NoBinding => {
                self.sweep.clear_streak(account_id);
                tracing::info!(
                    event = "release_sweep_deferred",
                    reason = "no_guardian_binding",
                    account_id = %account_id,
                    on_chain = %on_chain_commitment,
                    "Published storage carries no guardian binding; absence is not a switch"
                );
                record_outcome(ReleaseSweepOutcome::NoBinding);
                return Ok(VisitOutcome::Checked);
            }
            KeyVerdict::Base => {
                self.sweep.clear_streak(account_id);
                if let Some(guardian) = guardian_commitment.as_deref() {
                    self.report_own_key_mismatch(account_id, guardian, &own_commitment);
                }
                return Ok(VisitOutcome::Checked);
            }
            KeyVerdict::Own => {
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
                return Ok(VisitOutcome::Checked);
            }
            KeyVerdict::Foreign(commitment) => commitment,
        };

        let observations = match self.sweep.record_foreign_observation(
            account_id,
            &new_guardian_commitment,
            block_num,
        ) {
            Observation::Counted(observations) if observations >= self.confirmations => {
                self.release(
                    metadata,
                    &new_guardian_commitment,
                    ReleaseEvidence::ChainSweep {
                        on_chain_commitment: &on_chain_commitment,
                        stored_commitment: &stored.commitment,
                    },
                )
                .await?;
                return Ok(VisitOutcome::Checked);
            }
            Observation::Counted(observations) | Observation::NotNewer(observations) => {
                observations
            }
        };
        tracing::info!(
            event = "release_sweep_confirming",
            account_id = %account_id,
            new_guardian_commitment = %new_guardian_commitment,
            observations,
            confirmations = self.confirmations,
            block_num,
            "On-chain guardian key differs from this server's; awaiting \
             confirmation at a later block"
        );
        record_outcome(ReleaseSweepOutcome::Confirming);
        Ok(VisitOutcome::Checked)
    }

    /// Candidates that chain from the stored base: every pending proposal
    /// (whatever its label: the post-state's guardian key decides, not
    /// the client-written type) and every unpromoted delta
    /// canonicalization retained or the client abandoned (their proposal
    /// is gone, but their payload is not).
    async fn switch_candidates(
        &self,
        account_id: &str,
        stored: &StateObject,
    ) -> Result<Vec<(SwitchSource, serde_json::Value)>> {
        let proposals = self
            .state
            .storage
            .pull_pending_proposals(account_id)
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to pull pending proposals: {e}"))
            })?;
        let deltas = self
            .state
            .storage
            .pull_recoverable_deltas(account_id, self.abandoned_cutoff())
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to pull recoverable deltas: {e}"))
            })?;

        let mut candidates = Vec::new();
        for record in proposals {
            if record.proposal.prev_commitment != stored.commitment {
                continue;
            }
            let Some(tx_summary) = record.proposal.delta_payload.get("tx_summary").cloned() else {
                continue;
            };
            candidates.push((
                SwitchSource::Proposal {
                    proposal_id: canonical_proposal_id(&record.commitment),
                    storage_id: record.commitment,
                },
                tx_summary,
            ));
        }
        for DeltaObject {
            nonce,
            prev_commitment,
            delta_payload,
            ..
        } in deltas
        {
            if prev_commitment == stored.commitment {
                candidates.push((SwitchSource::Delta { nonce }, delta_payload));
            }
        }
        Ok(candidates)
    }

    /// Client-abandoned deltas stay in scope for the canonicalization
    /// retained TTL, as the reconcile pass keeps them.
    fn abandoned_cutoff(&self) -> chrono::DateTime<chrono::Utc> {
        let window = self
            .state
            .canonicalization
            .as_ref()
            .map_or(DEFAULT_ABANDONED_WINDOW_SECONDS, |config| {
                config.retained_ttl_seconds
            });
        i64::try_from(window)
            .ok()
            .and_then(chrono::Duration::try_seconds)
            .and_then(|window| self.state.clock.now().checked_sub_signed(window))
            .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC)
    }

    /// Look for a candidate whose post-state the chain reached: first at
    /// the head (free: the probe already read it), then in the account's
    /// transaction history, which finds the switch even after the account
    /// transacted again under its new guardian. Post-states are computed
    /// with the same `apply_delta` canonicalization uses and cached per
    /// candidate, failures included; history searches resume where the
    /// last one stopped.
    async fn find_executed_switch(
        &self,
        account_id: &str,
        stored: &StateObject,
        on_chain: &str,
        own_commitment: &str,
        base: &mut BaseGuardian,
    ) -> Result<SwitchSearch> {
        let candidates = self.switch_candidates(account_id, stored).await?;
        let current: HashSet<String> = candidates.iter().map(|(source, _)| source.key()).collect();
        self.sweep.retain_switches(account_id, &current);

        let mut ready = Vec::new();
        for (source, tx_summary) in candidates {
            let key = source.key();
            let evidence = match self
                .sweep
                .switch_evidence(account_id, &key, &stored.commitment)
            {
                Some(evidence) => evidence,
                None => {
                    let evidence = SwitchEvidence {
                        base_commitment: stored.commitment.clone(),
                        post_state: self
                            .precompute_switch(account_id, &key, tx_summary, stored)
                            .await,
                        resume_from_block: 0,
                    };
                    self.sweep
                        .remember_switch(account_id, &key, evidence.clone());
                    evidence
                }
            };
            if let SwitchPostState::Ready {
                post_commitment,
                guardian_commitment,
            } = evidence.post_state
            {
                ready.push((
                    source,
                    key,
                    post_commitment,
                    guardian_commitment,
                    evidence.resume_from_block,
                ));
            }
        }

        if let Some(position) = ready
            .iter()
            .position(|(_, _, post_commitment, _, _)| post_commitment == on_chain)
        {
            let (source, _, post_commitment, guardian_commitment, _) = ready.swap_remove(position);
            return Ok(SwitchSearch::Found(ExecutedSwitch {
                source,
                guardian_commitment,
                switch_commitment: post_commitment,
                switch_block_num: None,
            }));
        }

        let mut history_unavailable = false;
        for (source, key, post_commitment, guardian_commitment, resume_from_block) in ready {
            // Only a post-state that moves the guardian key away (from this
            // server's and from the stored base's) can release the account;
            // the rest are not worth a search.
            let verdict = self
                .classify_key(guardian_commitment.as_deref(), own_commitment, stored, base)
                .await?;
            if !matches!(verdict, KeyVerdict::Foreign(_)) {
                continue;
            }
            let search = self
                .state
                .network_client
                .find_transaction_ending_at(
                    account_id,
                    &post_commitment,
                    resume_from_block,
                    RpcReadMode::SingleAttempt,
                )
                .await;
            match search {
                Ok(search) => match search.found_in_block {
                    Some(block) => {
                        return Ok(SwitchSearch::Found(ExecutedSwitch {
                            source,
                            guardian_commitment,
                            switch_commitment: post_commitment,
                            switch_block_num: Some(block),
                        }));
                    }
                    None => {
                        self.sweep
                            .set_resume_block(account_id, &key, search.resume_from_block);
                    }
                },
                Err(e) => {
                    tracing::info!(
                        event = "release_sweep_deferred",
                        reason = "history_read_unavailable",
                        account_id = %account_id,
                        candidate = %key,
                        error = %e,
                        "Transaction history could not be searched for a candidate's \
                         post-state; falling back to the storage read"
                    );
                    history_unavailable = true;
                }
            }
        }
        Ok(if history_unavailable {
            SwitchSearch::HistoryUnavailable
        } else {
            SwitchSearch::NotFound
        })
    }

    async fn precompute_switch(
        &self,
        account_id: &str,
        key: &str,
        tx_summary: serde_json::Value,
        stored: &StateObject,
    ) -> SwitchPostState {
        let client = self.state.network_client.clone();
        let prev_state_json = stored.state_json.clone();
        let applied = crate::network::reconstructor()
            .run_background(move || {
                let (post_state, post_commitment) =
                    client.apply_delta(&prev_state_json, &tx_summary)?;
                let guardian_commitment = client.extract_guardian_commitment(&post_state)?;
                Ok::<_, String>((post_commitment, guardian_commitment))
            })
            .await;
        match applied {
            Ok((post_commitment, guardian_commitment)) => SwitchPostState::Ready {
                post_commitment,
                guardian_commitment,
            },
            Err(e) => {
                tracing::info!(
                    account_id = %account_id,
                    candidate = %key,
                    error = %GuardianError::from(e),
                    "Candidate does not apply to the stored base; it cannot serve as \
                     release evidence"
                );
                SwitchPostState::Unusable
            }
        }
    }

    async fn release_on_switch_match(
        &self,
        metadata: &AccountMetadata,
        stored: &StateObject,
        on_chain: &str,
        own_commitment: &str,
        executed: ExecutedSwitch,
        base: &mut BaseGuardian,
    ) -> Result<VisitOutcome> {
        let account_id = metadata.account_id.as_str();
        let verdict = self
            .classify_key(
                executed.guardian_commitment.as_deref(),
                own_commitment,
                stored,
                base,
            )
            .await?;
        let new_guardian_commitment = match verdict {
            KeyVerdict::Foreign(commitment) => commitment,
            // The executed transaction keeps the stored base's key, which
            // this server no longer holds: not a switch.
            KeyVerdict::Base => {
                self.sweep.clear_streak(account_id);
                if let Some(guardian) = executed.guardian_commitment.as_deref() {
                    self.report_own_key_mismatch(account_id, guardian, own_commitment);
                }
                return Ok(VisitOutcome::Checked);
            }
            // The executed transaction keeps this server's key: the account
            // is still bound here (the candidate is left for the push path
            // or reconcile).
            KeyVerdict::Own => {
                self.sweep.clear_streak(account_id);
                tracing::info!(
                    event = "release_sweep_deferred",
                    reason = "guardian_still_bound",
                    account_id = %account_id,
                    candidate = %executed.source.key(),
                    on_chain = %on_chain,
                    "A candidate executed but its post-state keeps this server's guardian key"
                );
                record_outcome(ReleaseSweepOutcome::StillBound);
                return Ok(VisitOutcome::Checked);
            }
            KeyVerdict::NoBinding => {
                self.sweep.clear_streak(account_id);
                tracing::info!(
                    event = "release_sweep_deferred",
                    reason = "no_guardian_binding",
                    account_id = %account_id,
                    candidate = %executed.source.key(),
                    on_chain = %on_chain,
                    "A candidate executed but its post-state carries no guardian binding; \
                     absence is not a switch"
                );
                record_outcome(ReleaseSweepOutcome::NoBinding);
                return Ok(VisitOutcome::Checked);
            }
        };

        let evidence = match &executed.source {
            SwitchSource::Proposal { proposal_id, .. } => ReleaseEvidence::ProposalMatch {
                proposal_id,
                on_chain_commitment: on_chain,
                stored_commitment: &stored.commitment,
                switch_commitment: &executed.switch_commitment,
                switch_block_num: executed.switch_block_num,
            },
            SwitchSource::Delta { nonce } => ReleaseEvidence::RecoverableDelta {
                delta_nonce: *nonce,
                on_chain_commitment: on_chain,
                stored_commitment: &stored.commitment,
                switch_commitment: &executed.switch_commitment,
                switch_block_num: executed.switch_block_num,
            },
        };
        let write = self
            .release(metadata, &new_guardian_commitment, evidence)
            .await?;
        // Resolve an executed proposal like canonicalization resolves a
        // matching one, but only once this call persisted the release: for
        // a private account the proposal is the only evidence there will
        // ever be, so a failed write (the `?` above) leaves it for the next
        // visit. A retained or abandoned delta row is left to the
        // reconcile pass and its TTL.
        if write == ReleaseWrite::Released
            && let SwitchSource::Proposal {
                storage_id,
                proposal_id,
            } = &executed.source
        {
            self.finalize_proposal(account_id, storage_id, proposal_id)
                .await;
        }
        Ok(VisitOutcome::Checked)
    }

    /// Release through the shared transition, which writes only while the
    /// stored state is still the one the evidence was proved against (a
    /// `/configure` or a promoted delta meanwhile voids it). A failed write
    /// is an error: the streak and the cached evidence are kept, so the
    /// next visit retries the write.
    async fn release(
        &self,
        metadata: &AccountMetadata,
        new_guardian_commitment: &str,
        evidence: ReleaseEvidence<'_>,
    ) -> Result<ReleaseWrite> {
        let account_id = metadata.account_id.as_str();
        let detected_by = evidence.detected_by();
        let write =
            release_switched_account(&self.state, metadata, new_guardian_commitment, evidence)
                .await;
        match write {
            ReleaseWrite::Released => {
                self.sweep.forget_account(account_id);
                tracing::info!(
                    event = "release_sweep_released",
                    account_id = %account_id,
                    detected_by,
                    new_guardian_commitment = %new_guardian_commitment,
                    "Released an account whose guardian switch the push path did not release"
                );
                record_outcome(ReleaseSweepOutcome::Released);
            }
            ReleaseWrite::AlreadyReleased => self.sweep.forget_account(account_id),
            ReleaseWrite::StateMoved => {
                self.sweep.forget_account(account_id);
                tracing::info!(
                    event = "release_sweep_deferred",
                    reason = "stored_base_moved",
                    account_id = %account_id,
                    detected_by,
                    "Stored state was replaced (a /configure or a promoted delta) before the \
                     release was written; re-evaluating from the new base next visit"
                );
            }
            ReleaseWrite::Failed => {
                return Err(GuardianError::StorageError(format!(
                    "Failed to persist release for switched account {account_id}"
                )));
            }
        }
        Ok(write)
    }

    /// Count the executed proposal as finalized (like the push paths, on
    /// detection) and delete it; a failed delete leaves it pending and is
    /// visible in the storage operation metrics.
    async fn finalize_proposal(&self, account_id: &str, storage_id: &str, proposal_id: &str) {
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
                "Resolved the pending proposal the chain proved executed"
            ),
            Err(e) => tracing::warn!(
                account_id = %account_id,
                proposal_id = %proposal_id,
                error = %e,
                "Failed to delete the executed proposal; it stays pending"
            ),
        }
    }
}

/// Process-now entry point for tests, demos and e2e endpoints: one walk of
/// the fleet, unpaced, releasing on the first observation.
pub async fn run_release_sweep_now(state: &AppState) -> Result<SweepSummary> {
    let sweeper = ReleaseSweeper::new(
        state.clone(),
        1,
        Duration::from_secs(60),
        Arc::new(SweepState::default()),
    );
    let mut summary = SweepSummary::default();
    let mut after: Option<String> = None;
    loop {
        let page = sweeper.list_page(after.as_deref(), 100).await?;
        let Some(last) = page.last().cloned() else {
            break;
        };
        for account_id in &page {
            summary.accounts += 1;
            if sweeper.visit_absorbing(account_id).await != VisitOutcome::Checked {
                summary.failed_accounts += 1;
            }
        }
        after = Some(last);
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::kinds;
    use crate::delta_object::{DeltaObject, DeltaStatus};
    use crate::metadata::auth::Auth;
    use crate::metadata::{AccountMetadata, NetworkConfig, ReleaseTransition};
    use crate::network::TransactionSearch;
    use crate::testing::helpers::{CapturingAuditor, create_test_app_state_with_mocks};
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use guardian_shared::SignatureScheme;

    const ACCOUNT: &str = "0xacc";
    const STORED: &str = "0xstored_commitment";
    const ON_CHAIN: &str = "0xchain_commitment";
    const POST_SWITCH: &str = "0xpost_switch_commitment";
    const FOREIGN: &str = "0xforeign_guardian";
    /// The guardian key the stored base carries when this server's own
    /// key has since changed.
    const PREVIOUS_OWN: &str = "0xprevious_own_guardian";
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

    fn visible(guardian: Option<&str>, nonce: u64, block_num: u32) -> OnChainGuardianBinding {
        OnChainGuardianBinding::Visible {
            on_chain_commitment: ON_CHAIN.to_string(),
            guardian_commitment: guardian.map(str::to_string),
            nonce,
            block_num,
        }
    }

    fn mismatch() -> std::result::Result<StateVerification, String> {
        Ok(StateVerification::Mismatch {
            on_chain: ON_CHAIN.into(),
        })
    }

    fn proposal_labeled(account_id: &str, prev_commitment: &str, label: &str) -> DeltaObject {
        DeltaObject {
            account_id: account_id.to_string(),
            nonce: 2,
            prev_commitment: prev_commitment.to_string(),
            // The mock storage derives a record's id from this field.
            new_commitment: Some(PROPOSAL_ID.to_string()),
            delta_payload: serde_json::json!({
                "tx_summary": {"data": "AAAA"},
                "signatures": [],
                "metadata": {"proposal_type": label}
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
        }
    }

    fn switch_proposal(account_id: &str, prev_commitment: &str) -> DeltaObject {
        proposal_labeled(account_id, prev_commitment, "switch_guardian")
    }

    /// A switch delta that reached this server but was retained by
    /// canonicalization (the chain had moved past its post-state).
    fn retained_delta(account_id: &str, nonce: u64, prev_commitment: &str) -> DeltaObject {
        DeltaObject {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: prev_commitment.to_string(),
            new_commitment: Some(POST_SWITCH.to_string()),
            delta_payload: serde_json::json!({"data": "AAAA"}),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::retained(
                "2026-09-01T00:00:00Z".to_string(),
                crate::delta_object::RetainReason::Diverged,
            ),
            metadata: None,
        }
    }

    /// A delta promoted on this server that produced `new_commitment`,
    /// with its ack signed by `ack` (unsigned when `None`).
    async fn canonical_delta(
        nonce: u64,
        new_commitment: &str,
        ack: Option<&crate::ack::AckRegistry>,
    ) -> DeltaObject {
        let delta = DeltaObject {
            account_id: ACCOUNT.to_string(),
            nonce,
            prev_commitment: "0xprevious_state".to_string(),
            new_commitment: Some(new_commitment.to_string()),
            delta_payload: crate::testing::helpers::create_test_delta_payload(
                "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b",
            ),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::canonical("2026-09-01T00:00:00Z".to_string()),
            metadata: None,
        };
        match ack {
            Some(ack) => ack
                .ack_delta(delta, &SignatureScheme::Falcon)
                .await
                .expect("ack signs"),
            None => delta,
        }
    }

    /// An ack registry holding a key this server does not hold now.
    async fn previous_ack_registry() -> crate::ack::AckRegistry {
        let dir =
            std::env::temp_dir().join(format!("guardian_previous_ack_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("keystore dir");
        crate::ack::AckRegistry::new(dir)
            .await
            .expect("ack registry")
    }

    fn found(block: u32) -> std::result::Result<TransactionSearch, String> {
        Ok(TransactionSearch {
            found_in_block: Some(block),
            resume_from_block: block + 1,
        })
    }

    fn not_found(resume_from_block: u32) -> std::result::Result<TransactionSearch, String> {
        Ok(TransactionSearch {
            found_in_block: None,
            resume_from_block,
        })
    }

    struct Harness {
        sweeper: ReleaseSweeper,
        network: Arc<MockNetworkClient>,
        metadata: Arc<MockMetadataStore>,
        storage: Arc<MockStorageBackend>,
        auditor: CapturingAuditor,
    }

    /// Sweeper over the given mocks. Mock queues pop LIFO, so callers
    /// queue responses in reverse order of use. The metadata row of
    /// `ACCOUNT` is served for every visit unless the caller queued rows.
    /// A visit peeks the stored commitment and pops a full state only
    /// when it needs one (the chain moved, or a stored state not yet
    /// checked). The stored state `STORED` starts out checked, as after
    /// any earlier visit; tests of the stored-state check forget it.
    fn harness(
        storage: MockStorageBackend,
        network: MockNetworkClient,
        metadata: MockMetadataStore,
        confirmations: u32,
    ) -> Harness {
        let metadata = if metadata.get_responses.lock().unwrap().is_empty() {
            metadata.with_get(Ok(Some(miden_meta(ACCOUNT))))
        } else {
            metadata
        };
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
            Duration::from_secs(60),
            Arc::new(SweepState::default()),
        );
        sweeper.sweep.mark_stored_checked(ACCOUNT, STORED);
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

    impl Harness {
        async fn visit(&self) -> VisitOutcome {
            self.sweeper
                .visit_account(ACCOUNT)
                .await
                .expect("visit succeeds")
        }
    }

    // --- Storage read ----------------------------------------------------

    #[tokio::test]
    async fn a_foreign_key_is_released_only_after_a_confirmation_at_a_later_block() {
        // Visit 1 (block 10): observation 1, re-check queued. Visit 2
        // reads block 10 again: the node has not moved on, nothing is
        // confirmed. Visit 3 (block 11): confirmed and released.
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_verify_commitment(mismatch())
                .with_verify_commitment(mismatch())
                .with_fetch_on_chain_guardian_binding(Ok(visible(Some(FOREIGN), 5, 11)))
                .with_fetch_on_chain_guardian_binding(Ok(visible(Some(FOREIGN), 5, 10)))
                .with_fetch_on_chain_guardian_binding(Ok(visible(Some(FOREIGN), 5, 10))),
            MockMetadataStore::new(),
            2,
        );

        assert_eq!(h.visit().await, VisitOutcome::Checked);
        assert!(released_ids(&h.metadata).is_empty(), "one observation");
        assert_eq!(h.sweeper.sweep_state().streak(ACCOUNT), Some(1));
        assert_eq!(
            h.sweeper.sweep_state().pending_rechecks(),
            vec![ACCOUNT.to_string()],
            "an open streak gets a re-check"
        );

        h.visit().await;
        assert!(
            released_ids(&h.metadata).is_empty(),
            "a second read of the same block confirms nothing"
        );
        assert_eq!(h.sweeper.sweep_state().streak(ACCOUNT), Some(1));

        h.visit().await;
        assert_eq!(released_ids(&h.metadata), vec![ACCOUNT.to_string()]);
        assert_eq!(
            h.metadata
                .set_released_expected_states
                .lock()
                .unwrap()
                .clone(),
            vec![STORED.to_string()],
            "the release is conditional on the base the evidence was proved against"
        );
        assert_eq!(h.sweeper.sweep_state().streak(ACCOUNT), None);
        assert!(h.sweeper.sweep_state().pending_rechecks().is_empty());
        let events = h.auditor.snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action_kind, kinds::ACCOUNTS_RELEASE);
        assert_eq!(events[0].payload["detected_by"], "chain_sweep");
        assert_eq!(events[0].payload["new_guardian_commitment"], FOREIGN);
        assert_eq!(events[0].payload["on_chain_commitment"], ON_CHAIN);
        assert_eq!(events[0].payload["stored_commitment"], STORED);
    }

    #[tokio::test]
    async fn a_read_older_than_the_stored_state_is_never_evidence() {
        // The stored state is at nonce 7 (re-onboarded after a switch
        // back, or not landed yet); the node serves a nonce-5 state with a
        // foreign key. Even with confirmations = 1 nothing is recorded.
        let h = harness(
            MockStorageBackend::new().with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_fetch_on_chain_guardian_binding(Ok(visible(Some(FOREIGN), 5, 90)))
                .with_account_nonce(Ok(Some(7))),
            MockMetadataStore::new(),
            1,
        );
        assert_eq!(h.visit().await, VisitOutcome::Checked);
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(h.sweeper.sweep_state().streak(ACCOUNT), None);
        assert!(h.sweeper.sweep_state().pending_rechecks().is_empty());
    }

    #[tokio::test]
    async fn a_read_at_or_past_the_stored_nonce_counts() {
        for on_chain_nonce in [7, 8] {
            let h = harness(
                MockStorageBackend::new().with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
                MockNetworkClient::new()
                    .with_verify_commitment(mismatch())
                    .with_fetch_on_chain_guardian_binding(Ok(visible(
                        Some(FOREIGN),
                        on_chain_nonce,
                        90,
                    )))
                    .with_account_nonce(Ok(Some(7))),
                MockMetadataStore::new(),
                1,
            );
            h.visit().await;
            assert_eq!(
                released_ids(&h.metadata),
                vec![ACCOUNT.to_string()],
                "on-chain nonce {on_chain_nonce} vs stored 7"
            );
        }
    }

    #[tokio::test]
    async fn an_unreadable_stored_nonce_fails_the_visit_instead_of_guessing() {
        let h = harness(
            MockStorageBackend::new().with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_fetch_on_chain_guardian_binding(Ok(visible(Some(FOREIGN), 5, 90)))
                .with_account_nonce(Err("corrupt state".into())),
            MockMetadataStore::new(),
            1,
        );
        assert_eq!(
            h.sweeper.visit_absorbing(ACCOUNT).await,
            VisitOutcome::Failed
        );
        assert!(released_ids(&h.metadata).is_empty());
    }

    #[tokio::test]
    async fn a_still_bound_lagging_account_is_never_released() {
        let h = harness(
            MockStorageBackend::new().with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
            MockNetworkClient::new().with_verify_commitment(mismatch()),
            MockMetadataStore::new(),
            1,
        );
        let own = h.sweeper.state.ack.commitment(&SignatureScheme::Falcon);
        h.network
            .fetch_on_chain_guardian_binding_responses
            .lock()
            .unwrap()
            .push(Ok(visible(Some(&own), 5, 90)));

        h.visit().await;
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(h.sweeper.sweep_state().streak(ACCOUNT), None);
        assert!(h.auditor.snapshot().is_empty());
    }

    #[tokio::test]
    async fn opaque_storage_and_missing_binding_never_release() {
        for binding in [OnChainGuardianBinding::Opaque, visible(None, 5, 90)] {
            let h = harness(
                MockStorageBackend::new().with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
                MockNetworkClient::new()
                    .with_verify_commitment(mismatch())
                    .with_fetch_on_chain_guardian_binding(Ok(binding.clone())),
                MockMetadataStore::new(),
                1,
            );
            assert_eq!(h.visit().await, VisitOutcome::Checked);
            assert!(
                released_ids(&h.metadata).is_empty(),
                "{binding:?} must never release"
            );
            assert_eq!(h.sweeper.sweep_state().streak(ACCOUNT), None);
        }
    }

    #[tokio::test]
    async fn a_chain_key_equal_to_the_stored_base_is_a_key_change_not_a_switch() {
        // This server's ack key changed after onboarding: the stored base
        // and the published storage both carry the previous key. The
        // chain moved (a lagging acknowledged delta), but nothing moved
        // the account away, so even confirmations = 1 records nothing.
        let h = harness(
            MockStorageBackend::new().with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_fetch_on_chain_guardian_binding(Ok(visible(Some(PREVIOUS_OWN), 5, 90)))
                .with_extract_guardian_commitment(Ok(Some(PREVIOUS_OWN.into()))),
            MockMetadataStore::new(),
            1,
        );
        assert_eq!(h.visit().await, VisitOutcome::Checked);
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(h.sweeper.sweep_state().streak(ACCOUNT), None);
        assert!(h.sweeper.sweep_state().pending_rechecks().is_empty());
        assert!(h.auditor.snapshot().is_empty());
    }

    /// Builds the harness for one metrics case.
    type BuildHarness = Box<dyn FnOnce() -> Harness>;

    /// The `guardian_release_sweep_accounts_total` lines one visit
    /// records, run under a local recorder.
    fn outcome_lines_after(build: impl FnOnce() -> Harness) -> Vec<String> {
        let recorder = crate::metrics::recorder::build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let h = build();
                    h.sweeper.visit_absorbing(ACCOUNT).await;
                })
        });
        handle
            .render()
            .lines()
            .filter(|line| line.starts_with("guardian_release_sweep_accounts_total{"))
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn every_finding_is_counted_under_its_documented_outcome() {
        // Dashboards, alerts and the troubleshooting guide key on these
        // labels; an own key read as the stored base's, for one, would
        // turn `still_bound` into a flood of `own_key_mismatch`.
        let storage =
            || MockStorageBackend::new().with_pull_state(Ok(stored_state(ACCOUNT, STORED)));
        let read = |binding: OnChainGuardianBinding| {
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_fetch_on_chain_guardian_binding(Ok(binding))
        };
        let cases: Vec<(&str, BuildHarness)> = vec![
            (
                "still_bound",
                Box::new(move || {
                    let h = harness(
                        storage(),
                        MockNetworkClient::new().with_verify_commitment(mismatch()),
                        MockMetadataStore::new(),
                        1,
                    );
                    let own = h.sweeper.state.ack.commitment(&SignatureScheme::Falcon);
                    h.network
                        .fetch_on_chain_guardian_binding_responses
                        .lock()
                        .unwrap()
                        .push(Ok(visible(Some(&own), 5, 90)));
                    h
                }),
            ),
            (
                "own_key_mismatch",
                Box::new(move || {
                    harness(
                        storage(),
                        read(visible(Some(PREVIOUS_OWN), 5, 90))
                            .with_extract_guardian_commitment(Ok(Some(PREVIOUS_OWN.into()))),
                        MockMetadataStore::new(),
                        1,
                    )
                }),
            ),
            (
                "no_binding",
                Box::new(move || {
                    harness(
                        storage(),
                        read(visible(None, 5, 90)),
                        MockMetadataStore::new(),
                        1,
                    )
                }),
            ),
            (
                "storage_opaque",
                Box::new(move || {
                    harness(
                        storage(),
                        read(OnChainGuardianBinding::Opaque),
                        MockMetadataStore::new(),
                        1,
                    )
                }),
            ),
            (
                "chain_behind_stored",
                Box::new(move || {
                    harness(
                        storage(),
                        read(visible(Some(FOREIGN), 5, 90)).with_account_nonce(Ok(Some(7))),
                        MockMetadataStore::new(),
                        1,
                    )
                }),
            ),
            (
                "probe_failed",
                Box::new(move || {
                    harness(
                        storage(),
                        MockNetworkClient::new().with_verify_commitment(Err("rpc down".into())),
                        MockMetadataStore::new(),
                        1,
                    )
                }),
            ),
            (
                "confirming",
                Box::new(move || {
                    harness(
                        storage(),
                        read(visible(Some(FOREIGN), 5, 90)),
                        MockMetadataStore::new(),
                        2,
                    )
                }),
            ),
            (
                "released",
                Box::new(move || {
                    harness(
                        storage(),
                        read(visible(Some(FOREIGN), 5, 90)),
                        MockMetadataStore::new(),
                        1,
                    )
                }),
            ),
        ];
        for (outcome, build) in cases {
            assert_eq!(
                outcome_lines_after(build),
                vec![format!(
                    "guardian_release_sweep_accounts_total{{outcome=\"{outcome}\"}} 1"
                )],
                "{outcome}"
            );
        }
    }

    #[tokio::test]
    async fn disagreeing_reads_are_not_an_observation() {
        let h = harness(
            MockStorageBackend::new().with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_fetch_on_chain_guardian_binding(Ok(OnChainGuardianBinding::Visible {
                    on_chain_commitment: STORED.into(),
                    guardian_commitment: Some(FOREIGN.into()),
                    nonce: 5,
                    block_num: 90,
                })),
            MockMetadataStore::new(),
            1,
        );
        h.visit().await;
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(h.sweeper.sweep_state().streak(ACCOUNT), None);
    }

    #[tokio::test]
    async fn probe_failures_defer_and_keep_the_streak_and_the_recheck() {
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
            MockNetworkClient::new()
                // visit 2: the storage read fails
                .with_verify_commitment(mismatch())
                .with_fetch_on_chain_guardian_binding(Err("rpc down".into()))
                // visit 1: the commitment probe fails
                .with_verify_commitment(Err("rpc down".into())),
            MockMetadataStore::new(),
            2,
        );
        h.sweeper
            .sweep_state()
            .record_foreign_observation(ACCOUNT, FOREIGN, 10);

        assert_eq!(
            h.sweeper.visit_absorbing(ACCOUNT).await,
            VisitOutcome::Deferred
        );
        assert_eq!(h.sweeper.sweep_state().streak(ACCOUNT), Some(1));
        assert_eq!(
            h.sweeper.sweep_state().pending_rechecks(),
            vec![ACCOUNT.to_string()]
        );
        assert_eq!(
            h.sweeper.visit_absorbing(ACCOUNT).await,
            VisitOutcome::Deferred
        );
        assert_eq!(h.sweeper.sweep_state().streak(ACCOUNT), Some(1));
        assert!(released_ids(&h.metadata).is_empty());
    }

    #[tokio::test]
    async fn a_failed_release_write_fails_the_visit_and_keeps_the_streak() {
        let h = harness(
            MockStorageBackend::new().with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_fetch_on_chain_guardian_binding(Ok(visible(Some(FOREIGN), 5, 90))),
            MockMetadataStore::new().with_set_released(Err("db down".into())),
            1,
        );
        assert_eq!(
            h.sweeper.visit_absorbing(ACCOUNT).await,
            VisitOutcome::Failed
        );
        assert_eq!(h.sweeper.sweep_state().streak(ACCOUNT), Some(1));
        assert_eq!(
            h.sweeper.sweep_state().pending_rechecks(),
            vec![ACCOUNT.to_string()],
            "the observation stands; a re-check retries the write"
        );
        assert!(h.auditor.snapshot().is_empty());
    }

    #[tokio::test]
    async fn a_state_replaced_before_the_write_voids_the_evidence() {
        // A /configure (or a promoted delta) replaced the stored state
        // between the read and the write: the store refuses, nothing is
        // audited, the streak closes.
        let h = harness(
            MockStorageBackend::new().with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_fetch_on_chain_guardian_binding(Ok(visible(Some(FOREIGN), 5, 90))),
            MockMetadataStore::new().with_set_released(Ok(ReleaseTransition::StateMoved)),
            1,
        );
        h.visit().await;
        assert_eq!(released_ids(&h.metadata), vec![ACCOUNT.to_string()]);
        assert!(h.auditor.snapshot().is_empty());
        assert_eq!(h.sweeper.sweep_state().streak(ACCOUNT), None);
        assert!(h.sweeper.sweep_state().pending_rechecks().is_empty());
    }

    // --- Proposal and delta match ------------------------------------------

    #[tokio::test]
    async fn a_switch_proposal_at_the_chain_head_releases_without_published_storage() {
        // A private account: the chain sits at the pending switch
        // proposal's post-state, which proves the switch executed:
        // released on the first visit, audited with the proposal id,
        // proposal finalized, no storage read, no history search.
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)])),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_apply_delta(Ok((serde_json::json!({"post": true}), ON_CHAIN.into())))
                .with_extract_guardian_commitment(Ok(Some(FOREIGN.into()))),
            MockMetadataStore::new(),
            2,
        );

        h.visit().await;
        assert_eq!(released_ids(&h.metadata), vec![ACCOUNT.to_string()]);
        assert!(
            h.network
                .get_fetch_on_chain_guardian_binding_calls()
                .is_empty()
        );
        assert!(h.network.get_find_transaction_ending_at_calls().is_empty());
        let events = h.auditor.snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["detected_by"], "proposal_match");
        assert_eq!(events[0].payload["proposal_id"], PROPOSAL_ID);
        assert_eq!(events[0].payload["new_guardian_commitment"], FOREIGN);
        assert_eq!(events[0].payload["on_chain_commitment"], ON_CHAIN);
        assert_eq!(events[0].payload["stored_commitment"], STORED);
        assert_eq!(events[0].payload["switch_commitment"], ON_CHAIN);
        assert!(events[0].payload["switch_block_num"].is_null());
        assert_eq!(
            h.storage.get_delete_delta_proposal_calls(),
            vec![(ACCOUNT.to_string(), PROPOSAL_ID.to_string())],
            "the executed switch proposal is finalized"
        );
        assert_eq!(h.sweeper.sweep_state().cached_switches(ACCOUNT), 0);
    }

    #[tokio::test]
    async fn a_proposal_under_any_label_is_evidence() {
        // A guardian rotation proposed through a custom-proposal API
        // carries its own label; the post-state's key decides, not the tag.
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_all_delta_proposals(Ok(vec![proposal_labeled(
                    ACCOUNT,
                    STORED,
                    "rotate_guardian",
                )])),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_apply_delta(Ok((serde_json::json!({"post": true}), ON_CHAIN.into())))
                .with_extract_guardian_commitment(Ok(Some(FOREIGN.into()))),
            MockMetadataStore::new(),
            2,
        );
        h.visit().await;
        assert_eq!(released_ids(&h.metadata), vec![ACCOUNT.to_string()]);
        assert_eq!(
            h.auditor.snapshot()[0].payload["detected_by"],
            "proposal_match"
        );
    }

    #[tokio::test]
    async fn a_switch_found_in_the_history_releases_after_the_account_moved_on() {
        // The new guardian already transacted: the chain head is past the
        // post-switch state, but the history holds the transaction that
        // ended at it.
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)])),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_apply_delta(Ok((serde_json::json!({"post": true}), POST_SWITCH.into())))
                .with_extract_guardian_commitment(Ok(Some(FOREIGN.into())))
                .with_find_transaction_ending_at(found(406_750)),
            MockMetadataStore::new(),
            2,
        );

        h.visit().await;
        assert_eq!(released_ids(&h.metadata), vec![ACCOUNT.to_string()]);
        assert_eq!(
            h.network.get_find_transaction_ending_at_calls(),
            vec![(ACCOUNT.to_string(), POST_SWITCH.to_string(), 0)],
            "the first search starts at genesis"
        );
        assert!(
            h.network
                .get_fetch_on_chain_guardian_binding_calls()
                .is_empty(),
            "a proven switch needs no storage read"
        );
        let events = h.auditor.snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["detected_by"], "proposal_match");
        assert_eq!(events[0].payload["on_chain_commitment"], ON_CHAIN);
        assert_eq!(events[0].payload["switch_commitment"], POST_SWITCH);
        assert_eq!(events[0].payload["switch_block_num"], 406_750);
        assert_eq!(h.storage.get_delete_delta_proposal_calls().len(), 1);
    }

    #[tokio::test]
    async fn a_retained_switch_delta_is_evidence_after_its_proposal_is_gone() {
        // The switch delta reached this server, canonicalization retained
        // it (the chain had already moved past its post-state) and deleted
        // the proposal on the way. The retained row still proves the switch.
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_recoverable_deltas(Ok(vec![retained_delta(ACCOUNT, 4, STORED)])),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_apply_delta(Ok((serde_json::json!({"post": true}), POST_SWITCH.into())))
                .with_extract_guardian_commitment(Ok(Some(FOREIGN.into())))
                .with_find_transaction_ending_at(found(88)),
            MockMetadataStore::new(),
            2,
        );
        h.visit().await;
        assert_eq!(released_ids(&h.metadata), vec![ACCOUNT.to_string()]);
        let events = h.auditor.snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["detected_by"], "recoverable_delta");
        assert_eq!(events[0].payload["delta_nonce"], 4);
        assert_eq!(events[0].payload["switch_commitment"], POST_SWITCH);
        assert_eq!(events[0].payload["switch_block_num"], 88);
        assert!(
            h.storage.get_delete_delta_proposal_calls().is_empty(),
            "there is no proposal to finalize; the row is the reconcile pass's"
        );
    }

    #[tokio::test]
    async fn history_searches_resume_where_the_last_one_stopped() {
        // Visit 1 searches blocks 0..=100 without a hit; visit 2 searches
        // from 101 and finds the switch. The post-state is computed once.
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)]))
                .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)])),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_verify_commitment(mismatch())
                .with_apply_delta(Ok((serde_json::json!({"post": true}), POST_SWITCH.into())))
                .with_extract_guardian_commitment(Ok(Some(FOREIGN.into())))
                .with_find_transaction_ending_at(found(140))
                .with_find_transaction_ending_at(not_found(101)),
            MockMetadataStore::new(),
            2,
        );

        h.visit().await;
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(h.sweeper.sweep_state().cached_switches(ACCOUNT), 1);
        h.visit().await;
        assert_eq!(released_ids(&h.metadata), vec![ACCOUNT.to_string()]);
        assert_eq!(
            h.network
                .get_find_transaction_ending_at_calls()
                .into_iter()
                .map(|(_, _, from)| from)
                .collect::<Vec<_>>(),
            vec![0, 101]
        );
        assert_eq!(
            h.network.apply_delta_responses.lock().unwrap().len(),
            0,
            "the single queued reconstruction was consumed once and cached"
        );
    }

    #[tokio::test]
    async fn an_unreadable_history_defers_a_private_account_and_keeps_the_proposal() {
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)])),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_apply_delta(Ok((serde_json::json!({"post": true}), POST_SWITCH.into())))
                .with_extract_guardian_commitment(Ok(Some(FOREIGN.into())))
                .with_find_transaction_ending_at(Err("rpc down".into()))
                .with_fetch_on_chain_guardian_binding(Ok(OnChainGuardianBinding::Opaque)),
            MockMetadataStore::new(),
            2,
        );
        assert_eq!(h.visit().await, VisitOutcome::Deferred);
        assert!(released_ids(&h.metadata).is_empty());
        assert!(h.storage.get_delete_delta_proposal_calls().is_empty());
        assert_eq!(h.sweeper.sweep_state().cached_switches(ACCOUNT), 1);
    }

    #[tokio::test]
    async fn an_unreadable_history_still_lets_the_storage_read_release_a_public_account() {
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)])),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_apply_delta(Ok((serde_json::json!({"post": true}), POST_SWITCH.into())))
                .with_extract_guardian_commitment(Ok(Some(FOREIGN.into())))
                .with_find_transaction_ending_at(Err("rpc down".into()))
                .with_fetch_on_chain_guardian_binding(Ok(visible(Some(FOREIGN), 5, 90))),
            MockMetadataStore::new(),
            1,
        );
        assert_eq!(h.visit().await, VisitOutcome::Checked);
        assert_eq!(released_ids(&h.metadata), vec![ACCOUNT.to_string()]);
        assert_eq!(
            h.auditor.snapshot()[0].payload["detected_by"],
            "chain_sweep"
        );
    }

    #[tokio::test]
    async fn a_candidate_that_keeps_the_stored_base_key_is_not_a_switch() {
        // This server's key changed after onboarding, and a proposal that
        // keeps the previous key executed: at the head it is not a
        // release, and off the head it is not worth a history search.
        for post_commitment in [ON_CHAIN, POST_SWITCH] {
            let h = harness(
                MockStorageBackend::new()
                    .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                    .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)])),
                MockNetworkClient::new()
                    .with_verify_commitment(mismatch())
                    .with_apply_delta(Ok((
                        serde_json::json!({"post": true}),
                        post_commitment.into(),
                    )))
                    .with_fetch_on_chain_guardian_binding(Ok(OnChainGuardianBinding::Opaque))
                    // The candidate's post-state key, then the stored
                    // base's (parsed lazily, once).
                    .with_extract_guardian_commitment(Ok(Some(PREVIOUS_OWN.into())))
                    .with_extract_guardian_commitment(Ok(Some(PREVIOUS_OWN.into()))),
                MockMetadataStore::new(),
                1,
            );
            assert_eq!(h.visit().await, VisitOutcome::Checked, "{post_commitment}");
            assert!(released_ids(&h.metadata).is_empty(), "{post_commitment}");
            assert!(h.auditor.snapshot().is_empty(), "{post_commitment}");
            assert!(
                h.network.get_find_transaction_ending_at_calls().is_empty(),
                "{post_commitment}"
            );
            assert!(
                h.storage.get_delete_delta_proposal_calls().is_empty(),
                "{post_commitment}"
            );
            assert!(
                h.network
                    .extract_guardian_commitment_responses
                    .lock()
                    .unwrap()
                    .is_empty(),
                "the base key was parsed once: {post_commitment}"
            );
        }
    }

    #[tokio::test]
    async fn a_proposal_that_keeps_this_servers_key_is_never_searched() {
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)])),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_apply_delta(Ok((serde_json::json!({"post": true}), POST_SWITCH.into()))),
            MockMetadataStore::new(),
            2,
        );
        let own = h.sweeper.state.ack.commitment(&SignatureScheme::Falcon);
        h.network
            .extract_guardian_commitment_responses
            .lock()
            .unwrap()
            .push(Ok(Some(own)));

        h.visit().await;
        assert!(h.network.get_find_transaction_ending_at_calls().is_empty());
        assert!(released_ids(&h.metadata).is_empty());
    }

    #[tokio::test]
    async fn an_executed_switch_to_no_key_is_no_binding_not_still_bound() {
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)])),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_apply_delta(Ok((serde_json::json!({"post": true}), ON_CHAIN.into())))
                .with_extract_guardian_commitment(Ok(None)),
            MockMetadataStore::new(),
            1,
        );
        assert_eq!(h.visit().await, VisitOutcome::Checked);
        assert!(released_ids(&h.metadata).is_empty());
        assert!(h.storage.get_delete_delta_proposal_calls().is_empty());
    }

    #[tokio::test]
    async fn a_failed_release_write_keeps_the_proposal_for_the_next_visit() {
        // Visit 1 proves the switch but the write fails: the proposal
        // must survive (for a private account it is the only evidence
        // there will ever be). Visit 2 matches it again from the cache
        // and releases.
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)]))
                .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)])),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_verify_commitment(mismatch())
                .with_apply_delta(Ok((serde_json::json!({"post": true}), ON_CHAIN.into())))
                .with_extract_guardian_commitment(Ok(Some(FOREIGN.into()))),
            MockMetadataStore::new()
                .with_set_released(Ok(ReleaseTransition::Released))
                .with_set_released(Err("transient db error".into())),
            2,
        );

        assert_eq!(
            h.sweeper.visit_absorbing(ACCOUNT).await,
            VisitOutcome::Failed,
            "visit 1 reports the failed write"
        );
        assert!(
            h.storage.get_delete_delta_proposal_calls().is_empty(),
            "an unpersisted release never finalizes the proposal"
        );
        assert!(h.auditor.snapshot().is_empty());

        assert_eq!(
            h.sweeper.visit_absorbing(ACCOUNT).await,
            VisitOutcome::Checked
        );
        assert_eq!(
            released_ids(&h.metadata),
            vec![ACCOUNT.to_string(), ACCOUNT.to_string()]
        );
        assert_eq!(h.auditor.snapshot().len(), 1);
        assert_eq!(h.storage.get_delete_delta_proposal_calls().len(), 1);
        assert_eq!(
            h.network.apply_delta_responses.lock().unwrap().len(),
            0,
            "the post-state was computed once"
        );
    }

    #[tokio::test]
    async fn a_race_already_released_by_the_push_path_finalizes_nothing() {
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)])),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_apply_delta(Ok((serde_json::json!({"post": true}), ON_CHAIN.into())))
                .with_extract_guardian_commitment(Ok(Some(FOREIGN.into()))),
            MockMetadataStore::new().with_set_released(Ok(ReleaseTransition::AlreadyReleased)),
            2,
        );
        assert_eq!(h.visit().await, VisitOutcome::Checked);
        assert!(
            h.storage.get_delete_delta_proposal_calls().is_empty(),
            "the writer that released it owns the cleanup and its count"
        );
        assert!(h.auditor.snapshot().is_empty());
    }

    #[tokio::test]
    async fn a_candidate_that_does_not_apply_is_cached_as_unusable() {
        // The reconstruction fails deterministically: it must not rerun
        // on every visit.
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)]))
                .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)])),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_verify_commitment(mismatch())
                // A second reconstruction would succeed and match; it must
                // never be attempted.
                .with_apply_delta(Ok((serde_json::json!({}), ON_CHAIN.into())))
                .with_apply_delta(Err("summary does not apply".into())),
            MockMetadataStore::new(),
            2,
        );
        h.visit().await;
        h.visit().await;
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(
            h.network.apply_delta_responses.lock().unwrap().len(),
            1,
            "the failed reconstruction ran once"
        );
        assert!(h.network.get_find_transaction_ending_at_calls().is_empty());
    }

    #[tokio::test]
    async fn evidence_of_candidates_that_are_gone_is_evicted() {
        // Visit 1 caches the proposal's post-state; by visit 2 the
        // proposal was deleted (by canonicalization or its users).
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_all_delta_proposals(Ok(vec![]))
                .with_pull_all_delta_proposals(Ok(vec![switch_proposal(ACCOUNT, STORED)])),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_verify_commitment(mismatch())
                .with_apply_delta(Ok((serde_json::json!({}), POST_SWITCH.into())))
                .with_extract_guardian_commitment(Ok(Some(FOREIGN.into()))),
            MockMetadataStore::new(),
            2,
        );
        h.visit().await;
        assert_eq!(h.sweeper.sweep_state().cached_switches(ACCOUNT), 1);
        h.visit().await;
        assert_eq!(h.sweeper.sweep_state().cached_switches(ACCOUNT), 0);
    }

    // --- Chain at the stored state ----------------------------------------

    #[tokio::test]
    async fn a_promoted_switch_whose_release_was_never_written_is_released() {
        // A switch delta was acknowledged and promoted here, but its
        // push-path release write failed: the stored state carries the new
        // guardian's key. Whether the chain still sits at it or the
        // account moved on under the new guardian, the sweep writes the
        // release the push path would have written, before any other read.
        for verification in [Ok(StateVerification::Match), mismatch()] {
            let h = harness(
                MockStorageBackend::new().with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
                MockNetworkClient::new()
                    .with_verify_commitment(verification.clone())
                    .with_extract_guardian_commitment(Ok(Some(FOREIGN.into()))),
                MockMetadataStore::new(),
                2,
            );
            h.sweeper.sweep_state().forget_stored_check(ACCOUNT);
            let promoted = canonical_delta(6, STORED, Some(&h.sweeper.state.ack)).await;
            h.storage
                .list_canonical_deltas_paged_responses
                .lock()
                .unwrap()
                .push(Ok(vec![promoted]));

            assert_eq!(h.visit().await, VisitOutcome::Checked, "{verification:?}");
            assert_eq!(released_ids(&h.metadata), vec![ACCOUNT.to_string()]);
            assert_eq!(
                h.metadata
                    .set_released_expected_states
                    .lock()
                    .unwrap()
                    .clone(),
                vec![STORED.to_string()]
            );
            let events = h.auditor.snapshot();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].payload["detected_by"], "delta");
            assert_eq!(events[0].payload["delta_nonce"], 6);
            assert_eq!(events[0].payload["new_commitment"], STORED);
            assert_eq!(events[0].payload["new_guardian_commitment"], FOREIGN);
            assert!(
                h.network
                    .get_fetch_on_chain_guardian_binding_calls()
                    .is_empty(),
                "the stored state alone is the evidence: {verification:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_foreign_stored_key_no_acknowledged_delta_explains_is_never_released() {
        // The stored key is not this server's, but no delta this server
        // acked with its current key produced the stored state: no delta at
        // all (the state came from /configure), a delta acked under a
        // previous key, an unsigned one, or a delta whose state a later
        // /configure replaced. That is this server's own key changing, not
        // a switch: never released, and parsed once while the chain holds
        // the stored state.
        let previous = previous_ack_registry().await;
        for case in [
            "no delta",
            "acked under a previous key",
            "unsigned",
            "state replaced since",
        ] {
            let h = harness(
                MockStorageBackend::new()
                    .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                    .with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
                MockNetworkClient::new()
                    .with_verify_commitment(Ok(StateVerification::Match))
                    .with_verify_commitment(Ok(StateVerification::Match))
                    .with_extract_guardian_commitment(Ok(Some(PREVIOUS_OWN.into()))),
                MockMetadataStore::new(),
                1,
            );
            h.sweeper.sweep_state().forget_stored_check(ACCOUNT);
            let latest = match case {
                "no delta" => vec![],
                "acked under a previous key" => {
                    vec![canonical_delta(6, STORED, Some(&previous)).await]
                }
                "unsigned" => vec![canonical_delta(6, STORED, None).await],
                _ => vec![canonical_delta(6, "0xreplaced_state", Some(&h.sweeper.state.ack)).await],
            };
            h.storage
                .list_canonical_deltas_paged_responses
                .lock()
                .unwrap()
                .push(Ok(latest));

            h.visit().await;
            h.visit().await;
            assert!(released_ids(&h.metadata).is_empty(), "{case}");
            assert!(h.auditor.snapshot().is_empty(), "{case}");
            assert_eq!(
                h.storage.pull_state_responses.lock().unwrap().len(),
                1,
                "only the first visit read the full state: {case}"
            );
        }
    }

    #[tokio::test]
    async fn an_unexplained_foreign_stored_key_off_the_chain_state_falls_through_unchecked() {
        // This server's key changed and the chain moved past the stored
        // state: the stored-state check proves nothing and reports nothing
        // (the detectors below do), and it stays unchecked so the key
        // change is reported once the chain holds the stored state. The
        // parsed key serves the rest of the visit.
        let h = harness(
            MockStorageBackend::new().with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
            MockNetworkClient::new()
                .with_verify_commitment(mismatch())
                .with_fetch_on_chain_guardian_binding(Ok(visible(Some(PREVIOUS_OWN), 5, 90)))
                .with_extract_guardian_commitment(Ok(Some(PREVIOUS_OWN.into()))),
            MockMetadataStore::new(),
            1,
        );
        h.sweeper.sweep_state().forget_stored_check(ACCOUNT);

        assert_eq!(h.visit().await, VisitOutcome::Checked);
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(h.sweeper.sweep_state().streak(ACCOUNT), None);
        assert!(
            h.network
                .extract_guardian_commitment_responses
                .lock()
                .unwrap()
                .is_empty(),
            "the stored key was parsed once for the whole visit"
        );
        assert!(!h.sweeper.sweep_state().is_stored_checked(ACCOUNT, STORED));
    }

    #[tokio::test]
    async fn a_verified_stored_state_is_parsed_once() {
        // Visit 1 parses the stored state (its key is this server's) and
        // remembers it; visit 2 of the same state reads only the
        // commitment.
        let h = harness(
            MockStorageBackend::new()
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED)))
                .with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
            MockNetworkClient::new()
                .with_verify_commitment(Ok(StateVerification::Match))
                .with_verify_commitment(Ok(StateVerification::Match)),
            MockMetadataStore::new(),
            2,
        );
        h.sweeper.sweep_state().forget_stored_check(ACCOUNT);
        let own = h.sweeper.state.ack.commitment(&SignatureScheme::Falcon);
        h.network
            .extract_guardian_commitment_responses
            .lock()
            .unwrap()
            .push(Ok(Some(own)));

        h.visit().await;
        h.visit().await;
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(
            h.storage.pull_state_responses.lock().unwrap().len(),
            1,
            "only the first visit read the full state"
        );
    }

    #[tokio::test]
    async fn chain_at_or_before_the_stored_base_clears_the_streak_and_the_cache() {
        for verification in [StateVerification::Match, StateVerification::Absent] {
            let h = harness(
                MockStorageBackend::new().with_pull_state(Ok(stored_state(ACCOUNT, STORED))),
                MockNetworkClient::new().with_verify_commitment(Ok(verification.clone())),
                MockMetadataStore::new(),
                1,
            );
            let sweep = h.sweeper.sweep_state();
            sweep.record_foreign_observation(ACCOUNT, FOREIGN, 10);
            sweep.schedule_recheck(ACCOUNT, Instant::now());
            sweep.remember_switch(
                ACCOUNT,
                "proposal:0xp",
                SwitchEvidence {
                    base_commitment: STORED.into(),
                    post_state: SwitchPostState::Unusable,
                    resume_from_block: 0,
                },
            );

            h.visit().await;
            assert!(
                h.network
                    .get_fetch_on_chain_guardian_binding_calls()
                    .is_empty()
            );
            assert!(released_ids(&h.metadata).is_empty());
            assert_eq!(sweep.streak(ACCOUNT), None, "{verification:?}");
            assert!(sweep.pending_rechecks().is_empty(), "{verification:?}");
            if verification == StateVerification::Absent {
                assert_eq!(sweep.cached_switches(ACCOUNT), 0, "{verification:?}");
            }
        }
    }

    // --- Visit bookkeeping -------------------------------------------------

    #[tokio::test]
    async fn the_visit_reads_the_row_fresh_instead_of_trusting_the_page() {
        // The rotation's page listed the account with no candidate in
        // flight; by the time it is visited the push path owns it.
        let mut busy = miden_meta(ACCOUNT);
        busy.has_pending_candidate = true;
        let h = harness(
            MockStorageBackend::new(),
            MockNetworkClient::new(),
            MockMetadataStore::new().with_get(Ok(Some(busy))),
            1,
        );
        h.visit().await;
        assert!(h.network.get_verify_commitment_calls().is_empty());
        assert_eq!(h.metadata.get_calls.lock().unwrap().clone(), vec![ACCOUNT]);
    }

    #[tokio::test]
    async fn evm_released_and_vanished_rows_are_not_probed() {
        let mut evm = miden_meta(ACCOUNT);
        evm.network_config = NetworkConfig::Evm {
            chain_id: 1,
            account_address: "0xabc".into(),
            multisig_validator_address: "0xdef".into(),
        };
        let mut released = miden_meta(ACCOUNT);
        released.released_at = Some(chrono::Utc::now());
        for row in [Some(evm), Some(released), None] {
            let h = harness(
                MockStorageBackend::new(),
                MockNetworkClient::new(),
                MockMetadataStore::new().with_get(Ok(row.clone())),
                1,
            );
            h.sweeper
                .sweep_state()
                .record_foreign_observation(ACCOUNT, FOREIGN, 10);
            h.visit().await;
            assert!(h.network.get_verify_commitment_calls().is_empty());
            assert_eq!(h.sweeper.sweep_state().streak(ACCOUNT), None, "{row:?}");
            assert!(released_ids(&h.metadata).is_empty());
        }
    }

    #[tokio::test]
    async fn process_now_walks_every_page_by_id() {
        let h = harness(
            MockStorageBackend::new(),
            MockNetworkClient::new(),
            MockMetadataStore::new()
                // page 3: empty (popped last)
                .with_list_release_sweep_ids(Ok(vec![]))
                // page 2
                .with_list_release_sweep_ids(Ok(vec!["0xb".into()]))
                // page 1
                .with_list_release_sweep_ids(Ok(vec!["0xa".into()])),
            1,
        );
        // Both accounts lack stored state in the mock: counted as failed
        // visits, but the walk completes.
        let summary = run_release_sweep_now(&h.sweeper.state).await.expect("walk");
        assert_eq!(summary.accounts, 2);
        assert_eq!(summary.failed_accounts, 2);
        assert_eq!(
            h.metadata.get_list_release_sweep_ids_calls(),
            vec![
                (None, 100),
                (Some("0xa".into()), 100),
                (Some("0xb".into()), 100)
            ]
        );
        assert_eq!(
            h.metadata.get_calls.lock().unwrap().clone(),
            vec!["0xa", "0xb"],
            "every visit reads its row fresh"
        );
    }

    // --- SweepState ------------------------------------------------------

    #[test]
    fn a_streak_counts_agreeing_keys_at_strictly_later_blocks() {
        let state = SweepState::default();
        assert_eq!(
            state.record_foreign_observation("a", "0xb", 10),
            Observation::Counted(1)
        );
        assert_eq!(
            state.record_foreign_observation("a", "0xb", 10),
            Observation::NotNewer(1)
        );
        assert_eq!(
            state.record_foreign_observation("a", "0xb", 9),
            Observation::NotNewer(1),
            "an older block never counts"
        );
        assert_eq!(
            state.record_foreign_observation("a", "0xb", 11),
            Observation::Counted(2)
        );
        assert_eq!(
            state.record_foreign_observation("a", "0xc", 12),
            Observation::Counted(1),
            "a different key restarts the streak"
        );
        state.clear_streak("a");
        assert_eq!(state.streak("a"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn rechecks_are_ordered_deduplicated_and_popped_when_due() {
        let state = SweepState::default();
        let now = Instant::now();
        state.schedule_recheck("b", now + Duration::from_secs(20));
        state.schedule_recheck("a", now + Duration::from_secs(10));
        state.schedule_recheck("b", now + Duration::from_secs(5));
        assert_eq!(
            state.pending_rechecks(),
            vec!["a", "b"],
            "one entry per account"
        );
        assert_eq!(
            state.next_recheck_due(),
            Some(now + Duration::from_secs(10))
        );
        assert_eq!(state.pop_due_recheck(now), None, "nothing due yet");
        assert_eq!(
            state
                .pop_due_recheck(now + Duration::from_secs(10))
                .as_deref(),
            Some("a")
        );
        state.record_foreign_observation("b", "0xk", 1);
        state.clear_streak("b");
        assert!(
            state.pending_rechecks().is_empty(),
            "closing a streak drops its re-check"
        );
    }

    #[test]
    fn proposal_ids_are_canonicalized_for_the_audit_trail() {
        assert_eq!(canonical_proposal_id("abcdef"), "0xabcdef");
        assert_eq!(canonical_proposal_id("0xabcdef"), "0xabcdef");
    }
}
