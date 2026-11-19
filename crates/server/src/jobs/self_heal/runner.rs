//! The runner: reads the chain and storage, calls the planner, applies
//! each plan and writes the recovery file.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use guardian_shared::SignatureScheme;
use tokio_util::sync::CancellationToken;

use super::plan::plan_account;
use super::report::RecoveryReport;
use super::types::{
    AccountInputs, AccountPlan, ChainInfo, ChainRead, CreationDeltaInfo, HealPlan, NeededInfo,
    RecoveryReason, ReplayInfo, RowKind, RowVerdict, SelfHealConfig, SelfHealMode, SelfHealTotals,
    StoredInfo, UnrecoveredAccount,
};
use crate::coordination::{LeaderElector, Lease};
use crate::jobs::canonicalization::spawn_lease_renewal;
use crate::metadata::{AccountMetadata, NetworkConfig, ReleaseTransition};
use crate::network::{NetworkClient, ReconstructError, RpcReadMode, StateVerification};
use crate::state::AppState;
use crate::state_object::StateObject;
use crate::storage::{EVERY_ARCHIVED_DELTA, HealWriteOutcome, is_storage_not_found};

/// The holder renews every third of the TTL, like the other leased tasks.
const LEASE_TTL: Duration = Duration::from_secs(30);
const LEASE_RENEW_INTERVAL: Duration = Duration::from_secs(10);
const LEASE_RETRY_INTERVAL: Duration = Duration::from_secs(5);

const LIST_PAGE_SIZE: u32 = 500;

/// Run the self-heal block once. It returns an error when the block could
/// not complete; the caller then refuses to start the server.
pub async fn run_self_heal(
    state: &AppState,
    leader: Arc<dyn LeaderElector>,
    config: &SelfHealConfig,
) -> Result<SelfHealTotals, String> {
    run_with_planner(state, leader, config, plan_account).await
}

/// The modes in which the block runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveMode {
    DryRun,
    Apply,
}

impl ActiveMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::DryRun => "dry-run",
            Self::Apply => "apply",
        }
    }
}

/// What the run did with one account.
enum Visit {
    Healthy,
    Skipped,
    Healed,
    /// Not recovered. `None` when the account has no recovery-file entry:
    /// its stored state moved before the heal write.
    NotRecovered(Option<Box<UnrecoveredAccount>>),
}

async fn run_with_planner<P>(
    state: &AppState,
    leader: Arc<dyn LeaderElector>,
    config: &SelfHealConfig,
    planner: P,
) -> Result<SelfHealTotals, String>
where
    P: Fn(&dyn NetworkClient, AccountInputs) -> AccountPlan + Send + Sync + 'static,
{
    let mode = match config.mode {
        SelfHealMode::Off => {
            tracing::info!("self-heal is off");
            return Ok(SelfHealTotals::default());
        }
        SelfHealMode::DryRun => ActiveMode::DryRun,
        SelfHealMode::Apply => ActiveMode::Apply,
    };
    tracing::info!(
        mode = mode.as_str(),
        recovery_file = %config.recovery_file.display(),
        max_concurrent_accounts = config.max_concurrent_accounts,
        "self-heal starting; waiting for the canonicalization lease"
    );

    let lease = acquire_lease(leader.as_ref()).await;
    let lease_lost = CancellationToken::new();
    let renewal = spawn_lease_renewal(
        leader.clone(),
        lease.clone(),
        LEASE_TTL,
        LEASE_RENEW_INTERVAL,
        lease_lost.clone(),
        "Self-heal",
    );
    let result = {
        let _stop_renewal = lease_lost.clone().drop_guard();
        let run = Run {
            state,
            mode,
            planner: Arc::new(planner),
            lease_lost: lease_lost.clone(),
        };
        run.heal_all(config).await
    };
    if let Err(error) = renewal.await {
        tracing::warn!(error = %error, "self-heal lease renewal task failed");
    }
    if let Err(error) = leader.release(lease).await {
        tracing::warn!(error = %error, "failed to release the canonicalization lease after self-heal");
    }

    match &result {
        Ok(totals) => tracing::info!(
            mode = mode.as_str(),
            accounts = totals.accounts,
            healthy = totals.healthy,
            skipped = totals.skipped,
            healed = totals.healed,
            not_recovered = totals.not_recovered,
            recovery_file = %config.recovery_file.display(),
            "self-heal finished"
        ),
        Err(error) => tracing::error!(mode = mode.as_str(), error = %error, "self-heal failed"),
    }
    result
}

async fn acquire_lease(leader: &dyn LeaderElector) -> Lease {
    loop {
        match leader.try_acquire(LEASE_TTL).await {
            Ok(Some(lease)) => {
                tracing::info!(holder = %lease.holder_id, "self-heal holds the canonicalization lease");
                return lease;
            }
            Ok(None) => tracing::info!(
                "another replica holds the canonicalization lease; self-heal waits for it"
            ),
            Err(error) => tracing::warn!(
                error = %error,
                "failed to acquire the canonicalization lease for self-heal; retrying"
            ),
        }
        tokio::time::sleep(LEASE_RETRY_INTERVAL).await;
    }
}

struct Run<'a, P> {
    state: &'a AppState,
    mode: ActiveMode,
    planner: Arc<P>,
    /// Cancelled when the lease is lost. No write starts after that.
    lease_lost: CancellationToken,
}

impl<P> Run<'_, P>
where
    P: Fn(&dyn NetworkClient, AccountInputs) -> AccountPlan + Send + Sync + 'static,
{
    async fn heal_all(&self, config: &SelfHealConfig) -> Result<SelfHealTotals, String> {
        let accounts = self.list_accounts().await?;
        let mut totals = SelfHealTotals {
            accounts: accounts.len(),
            ..SelfHealTotals::default()
        };
        let mut unrecovered = Vec::new();
        {
            let mut visits = futures::stream::iter(accounts)
                .map(|metadata| self.visit(metadata))
                .buffer_unordered(config.max_concurrent_accounts.max(1));
            while let Some(visit) = visits.next().await {
                match visit? {
                    Visit::Healthy => totals.healthy += 1,
                    Visit::Skipped => totals.skipped += 1,
                    Visit::Healed => totals.healed += 1,
                    Visit::NotRecovered(entry) => {
                        totals.not_recovered += 1;
                        unrecovered.extend(entry.map(|entry| *entry));
                    }
                }
            }
        }

        let report = RecoveryReport::new(
            self.state.clock.now_rfc3339(),
            self.state.dashboard.network_type().to_string(),
            self.mode == ActiveMode::DryRun,
            totals.clone(),
            unrecovered,
        );
        let path = config.recovery_file.clone();
        tokio::task::spawn_blocking(move || report.write_atomic(&path))
            .await
            .map_err(|error| format!("self-heal: the recovery file task failed: {error}"))??;
        Ok(totals)
    }

    /// Every account. The paged listing can skip a row that another
    /// writer moves during the walk, so the id listing fills any gap.
    async fn list_accounts(&self) -> Result<Vec<AccountMetadata>, String> {
        let metadata = &self.state.metadata;
        let mut seen = HashSet::new();
        let mut accounts = Vec::new();
        for account in metadata
            .list_all(LIST_PAGE_SIZE)
            .await
            .map_err(|error| format!("self-heal: failed to list accounts: {error}"))?
        {
            if seen.insert(account.account_id.clone()) {
                accounts.push(account);
            }
        }
        let ids = metadata
            .list()
            .await
            .map_err(|error| format!("self-heal: failed to list account ids: {error}"))?;
        for id in ids {
            if !seen.insert(id.clone()) {
                continue;
            }
            let account = metadata
                .get(&id)
                .await
                .map_err(|error| format!("self-heal: failed to read account {id}: {error}"))?;
            accounts.extend(account);
        }
        Ok(accounts)
    }

    async fn visit(&self, metadata: AccountMetadata) -> Result<Visit, String> {
        let account_id = metadata.account_id.clone();
        match &metadata.network_config {
            NetworkConfig::Miden { .. } => {}
            NetworkConfig::Evm { .. } => return Ok(Visit::Skipped),
        }
        if metadata.released_at.is_some() {
            return Ok(Visit::Skipped);
        }
        let stored = match self.state.storage.pull_state(&account_id).await {
            Ok(stored) => stored,
            Err(error) if is_storage_not_found(&error) => {
                tracing::warn!(
                    account_id = %account_id,
                    "self-heal skips an account that has no stored state"
                );
                return Ok(Visit::Skipped);
            }
            Err(error) => {
                return Err(format!(
                    "self-heal: failed to read the state of account {account_id}: {error}"
                ));
            }
        };

        let verification = self
            .state
            .network_client
            .verify_commitment(&account_id, &stored.commitment, RpcReadMode::Configured)
            .await;
        let chain = match verification {
            Ok(StateVerification::Match) => return Ok(Visit::Healthy),
            Ok(StateVerification::Mismatch { on_chain }) => ChainRead::Mismatch {
                commitment: on_chain,
            },
            Ok(StateVerification::Absent) => ChainRead::Absent,
            Err(error) => return self.chain_unreadable(&metadata, &stored, &error),
        };

        let stored_commitment = stored.commitment.clone();
        let inputs = self.load_inputs(metadata, stored, chain).await?;
        let plan = self.plan(&account_id, inputs).await?;
        log_plan(&account_id, &plan, self.mode);
        match self.mode {
            ActiveMode::DryRun => Ok(dry_run_visit(plan)),
            ActiveMode::Apply => self.apply(&account_id, &stored_commitment, plan).await,
        }
    }

    fn chain_unreadable(
        &self,
        metadata: &AccountMetadata,
        stored: &StateObject,
        error: &str,
    ) -> Result<Visit, String> {
        match self.mode {
            ActiveMode::Apply => Err(format!(
                "self-heal: the chain read failed for account {}: {error}",
                metadata.account_id
            )),
            ActiveMode::DryRun => {
                tracing::warn!(
                    account_id = %metadata.account_id,
                    error = %error,
                    "self-heal could not read the chain for the account; apply mode would stop here"
                );
                Ok(Visit::NotRecovered(Some(Box::new(rpc_error_entry(
                    metadata, stored,
                )))))
            }
        }
    }

    async fn load_inputs(
        &self,
        metadata: AccountMetadata,
        stored: StateObject,
        chain: ChainRead,
    ) -> Result<AccountInputs, String> {
        let storage = &self.state.storage;
        let account_id = metadata.account_id.as_str();
        let deltas = storage
            .pull_deltas_after(account_id, 0)
            .await
            .map_err(|error| {
                format!("self-heal: failed to read the deltas of account {account_id}: {error}")
            })?;
        let superseded = storage
            .pull_superseded_deltas(account_id, EVERY_ARCHIVED_DELTA)
            .await
            .map_err(|error| {
                format!(
                    "self-heal: failed to read the archived deltas of account {account_id}: {error}"
                )
            })?;
        let proposals = storage
            .pull_all_delta_proposals(account_id)
            .await
            .map_err(|error| {
                format!("self-heal: failed to read the proposals of account {account_id}: {error}")
            })?;
        Ok(AccountInputs {
            metadata,
            stored,
            chain,
            deltas,
            superseded,
            proposals,
            now: self.state.clock.now_rfc3339(),
        })
    }

    async fn plan(&self, account_id: &str, inputs: AccountInputs) -> Result<AccountPlan, String> {
        let planner = self.planner.clone();
        let client = self.state.network_client.clone();
        crate::network::reconstructor()
            .run_background(move || Ok(planner(client.as_ref(), inputs)))
            .await
            .map_err(|error| match error {
                ReconstructError::Operation(message) | ReconstructError::Task(message) => {
                    format!("self-heal: planning account {account_id} failed: {message}")
                }
            })
    }

    async fn apply(
        &self,
        account_id: &str,
        stored_commitment: &str,
        plan: AccountPlan,
    ) -> Result<Visit, String> {
        match plan {
            AccountPlan::Skip { .. } => Ok(Visit::Skipped),
            AccountPlan::Heal(plan) => self.apply_heal(*plan).await,
            AccountPlan::Unrecovered(entry) => {
                self.apply_unrecovered(account_id, stored_commitment, entry)
                    .await
            }
        }
    }

    async fn apply_heal(&self, plan: HealPlan) -> Result<Visit, String> {
        self.ensure_lease()?;
        let HealPlan {
            write,
            guardian_commitment,
            ..
        } = plan;
        let account_id = write.account_id.clone();
        let true_commitment = write.state.commitment.clone();
        let outcome = self
            .state
            .storage
            .apply_account_heal(self.state.metadata.as_ref(), write)
            .await
            .map_err(|error| {
                format!("self-heal: the heal write failed for account {account_id}: {error}")
            })?;
        match outcome {
            HealWriteOutcome::Applied => {}
            HealWriteOutcome::StateMoved { current } => {
                tracing::warn!(
                    account_id = %account_id,
                    current_commitment = %current,
                    "self-heal did not write: the stored state moved after the plan; run self-heal again"
                );
                return Ok(Visit::NotRecovered(None));
            }
        }
        tracing::info!(
            account_id = %account_id,
            commitment = %true_commitment,
            "self-heal healed the account"
        );
        let foreign_guardian = guardian_commitment.filter(|guardian| !self.is_own_key(guardian));
        if let Some(guardian) = foreign_guardian {
            tracing::info!(
                account_id = %account_id,
                guardian_commitment = %guardian,
                "the true state binds another guardian; self-heal releases the account"
            );
            self.release(&account_id, &true_commitment).await?;
        }
        Ok(Visit::Healed)
    }

    async fn apply_unrecovered(
        &self,
        account_id: &str,
        stored_commitment: &str,
        entry: Box<UnrecoveredAccount>,
    ) -> Result<Visit, String> {
        self.ensure_lease()?;
        // The proposals are deleted only together with the pause. An
        // account that is not paused is not treated as forged.
        let deleted_proposals = match entry.pause {
            true => self.delete_pending_proposals(account_id).await?,
            false => 0,
        };
        if entry.release {
            self.release(account_id, stored_commitment).await?;
        }
        if entry.pause {
            let reason = format!("self-heal: {}", reason_label(entry.reason));
            self.state
                .metadata
                .set_pause(account_id, self.state.clock.now(), &reason)
                .await
                .map_err(|error| {
                    format!("self-heal: failed to pause account {account_id}: {error}")
                })?;
        }
        tracing::warn!(
            account_id = %account_id,
            reason = reason_label(entry.reason),
            deleted_proposals,
            paused = entry.pause,
            released = entry.release,
            "self-heal did not recover the account"
        );
        Ok(Visit::NotRecovered(Some(entry)))
    }

    async fn delete_pending_proposals(&self, account_id: &str) -> Result<usize, String> {
        let storage = &self.state.storage;
        let pending = storage
            .pull_pending_proposals(account_id)
            .await
            .map_err(|error| {
                format!(
                    "self-heal: failed to read the pending proposals of account {account_id}: {error}"
                )
            })?;
        for record in &pending {
            storage
                .delete_delta_proposal(account_id, &record.commitment)
                .await
                .map_err(|error| {
                    format!(
                        "self-heal: failed to delete proposal {} of account {account_id}: {error}",
                        record.commitment
                    )
                })?;
        }
        Ok(pending.len())
    }

    async fn release(&self, account_id: &str, commitment: &str) -> Result<(), String> {
        let transition = self
            .state
            .metadata
            .set_released_if_state(
                account_id,
                self.state.clock.now(),
                commitment,
                self.state.storage.as_ref(),
            )
            .await
            .map_err(|error| {
                format!("self-heal: failed to release account {account_id}: {error}")
            })?;
        match transition {
            ReleaseTransition::Released | ReleaseTransition::AlreadyReleased => {
                tracing::info!(account_id = %account_id, "self-heal released the account")
            }
            ReleaseTransition::StateMoved => tracing::warn!(
                account_id = %account_id,
                "self-heal did not release the account: its stored state moved"
            ),
        }
        Ok(())
    }

    fn ensure_lease(&self) -> Result<(), String> {
        if self.lease_lost.is_cancelled() {
            return Err(
                "self-heal lost the canonicalization lease; it stops before the next write"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn is_own_key(&self, commitment: &str) -> bool {
        [SignatureScheme::Falcon, SignatureScheme::Ecdsa]
            .iter()
            .any(|scheme| {
                self.state
                    .ack
                    .commitment(scheme)
                    .eq_ignore_ascii_case(commitment)
            })
    }
}

fn dry_run_visit(plan: AccountPlan) -> Visit {
    match plan {
        AccountPlan::Skip { .. } => Visit::Skipped,
        AccountPlan::Heal(_) => Visit::Healed,
        AccountPlan::Unrecovered(entry) => Visit::NotRecovered(Some(entry)),
    }
}

fn log_plan(account_id: &str, plan: &AccountPlan, mode: ActiveMode) {
    match plan {
        AccountPlan::Skip { reason } => tracing::info!(
            account_id = %account_id,
            mode = mode.as_str(),
            outcome = "skip",
            reason = %reason,
            "self-heal plan"
        ),
        AccountPlan::Heal(plan) => {
            tracing::info!(
                account_id = %account_id,
                mode = mode.as_str(),
                outcome = "heal",
                commitment = %plan.write.state.commitment,
                true_signers = ?plan.true_signers,
                guardian_commitment = ?plan.guardian_commitment,
                delta_writes = plan.write.delta_writes.len(),
                proposal_writes = plan.write.proposal_writes.len(),
                "self-heal plan"
            );
            for action in &plan.actions {
                tracing::info!(
                    account_id = %account_id,
                    mode = mode.as_str(),
                    row = row_kind_label(action.row),
                    id = %action.id,
                    verdict = verdict_label(action.verdict),
                    reason = %action.reason,
                    "self-heal row"
                );
            }
        }
        AccountPlan::Unrecovered(entry) => tracing::warn!(
            account_id = %account_id,
            mode = mode.as_str(),
            outcome = "not_recovered",
            reason = reason_label(entry.reason),
            detail = ?entry.detail,
            chain_status = %entry.chain.status,
            delta_rows = entry.delta_rows.len(),
            proposals = entry.proposals.len(),
            pause = entry.pause,
            release = entry.release,
            "self-heal plan"
        ),
    }
}

/// The recovery-file entry of an account whose chain read failed in a dry
/// run. No row was read, so the row fields are empty.
fn rpc_error_entry(metadata: &AccountMetadata, stored: &StateObject) -> UnrecoveredAccount {
    let mut signers = metadata.auth.cosigner_commitments();
    signers.sort();
    UnrecoveredAccount {
        account_id: metadata.account_id.clone(),
        reason: RecoveryReason::RpcError,
        detail: Some("the chain read failed; the server log has the error".to_string()),
        chain: ChainInfo {
            status: "unreadable".to_string(),
            commitment: None,
        },
        stored: StoredInfo {
            commitment: stored.commitment.clone(),
            nonce: stored.nonce,
            signers,
            created_at: stored.created_at.clone(),
            updated_at: stored.updated_at.clone(),
        },
        creation_delta: CreationDeltaInfo {
            status: "not_read".to_string(),
            nonce: None,
            new_commitment: None,
            signers: None,
        },
        replay: ReplayInfo {
            base: "none".to_string(),
            rows_applied: 0,
            last_commitment: None,
            last_nonce: None,
            last_signers: None,
        },
        needed: NeededInfo {
            state_with_commitment: None,
        },
        delta_rows: Vec::new(),
        proposals: Vec::new(),
        pause: false,
        release: false,
    }
}

fn reason_label(reason: RecoveryReason) -> &'static str {
    match reason {
        RecoveryReason::NoBase => "no_base",
        RecoveryReason::CreationDeltaPending => "creation_delta_pending",
        RecoveryReason::ReplayShort => "replay_short",
        RecoveryReason::LeftThisGuardian => "left_this_guardian",
        RecoveryReason::AmbiguousHistory => "ambiguous_history",
        RecoveryReason::UndecodableRow => "undecodable_row",
        RecoveryReason::RpcError => "rpc_error",
    }
}

fn row_kind_label(kind: RowKind) -> &'static str {
    match kind {
        RowKind::Delta => "delta",
        RowKind::Proposal => "proposal",
    }
}

fn verdict_label(verdict: RowVerdict) -> &'static str {
    match verdict {
        RowVerdict::Canonical => "canonical",
        RowVerdict::Keep => "keep",
        RowVerdict::KeepWithFewerSignatures => "keep_with_fewer_signatures",
        RowVerdict::Retain => "retain",
        RowVerdict::Delete => "delete",
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use chrono::{DateTime, Utc};

    use super::*;
    use crate::delta_object::{DeltaObject, DeltaStatus};
    use crate::metadata::auth::Auth;
    use crate::metadata::{
        AccountListCursor, ClearTransition, MetadataStore, ReleaseSweepMembership,
        SessionFloorOutcome,
    };
    use crate::services::account_status::PauseTransition;
    use crate::storage::{AccountHealWrite, HealDeltaWrite};
    use crate::testing::helpers::create_test_app_state_with_mocks;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};

    const ACCOUNT: &str = "0x0a0000000000000000000000000000";
    const FORGED: &str = "0xforged";
    const ON_CHAIN: &str = "0xonchain";

    /// A metadata store that records each pause and how many proposals
    /// were deleted before it. Every other call goes to the mock.
    #[derive(Clone)]
    struct RecordingMetadata {
        inner: MockMetadataStore,
        storage: MockStorageBackend,
        pauses: Arc<Mutex<Vec<(String, String, usize)>>>,
    }

    impl RecordingMetadata {
        fn pauses(&self) -> Vec<(String, String, usize)> {
            self.pauses.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl MetadataStore for RecordingMetadata {
        async fn get(&self, account_id: &str) -> Result<Option<AccountMetadata>, String> {
            self.inner.get(account_id).await
        }
        async fn set(&self, metadata: AccountMetadata) -> Result<(), String> {
            self.inner.set(metadata).await
        }
        async fn create(&self, metadata: AccountMetadata) -> Result<bool, String> {
            self.inner.create(metadata).await
        }
        async fn list(&self) -> Result<Vec<String>, String> {
            self.inner.list().await
        }
        async fn count(&self) -> Result<u64, String> {
            self.inner.count().await
        }
        async fn get_for_read(&self, account_id: &str) -> Result<Option<AccountMetadata>, String> {
            self.inner.get_for_read(account_id).await
        }
        async fn get_for_read_at(
            &self,
            account_id: &str,
            auth_version: i64,
        ) -> Result<Option<AccountMetadata>, String> {
            self.inner.get_for_read_at(account_id, auth_version).await
        }
        async fn list_paged(
            &self,
            limit: u32,
            cursor: Option<AccountListCursor>,
            paused: Option<bool>,
        ) -> Result<Vec<AccountMetadata>, String> {
            self.inner.list_paged(limit, cursor, paused).await
        }
        async fn list_with_pending_candidates(&self) -> Result<Vec<String>, String> {
            self.inner.list_with_pending_candidates().await
        }
        async fn list_release_sweep_ids(
            &self,
            after: Option<&str>,
            limit: u32,
        ) -> Result<Vec<String>, String> {
            self.inner.list_release_sweep_ids(after, limit).await
        }
        async fn update_last_auth_timestamp_cas(
            &self,
            account_id: &str,
            signer_commitment: &str,
            new_timestamp: i64,
        ) -> Result<bool, String> {
            self.inner
                .update_last_auth_timestamp_cas(account_id, signer_commitment, new_timestamp)
                .await
        }
        async fn advance_session_floor(
            &self,
            account_id: &str,
            floor_key: &str,
            timestamp: i64,
            skew_ms: i64,
            clock: &dyn crate::builder::clock::Clock,
        ) -> Result<SessionFloorOutcome, String> {
            self.inner
                .advance_session_floor(account_id, floor_key, timestamp, skew_ms, clock)
                .await
        }
        async fn purge_session_floors(
            &self,
            max_age_ms: i64,
            clock: &dyn crate::builder::clock::Clock,
        ) -> Result<u64, String> {
            self.inner.purge_session_floors(max_age_ms, clock).await
        }
        async fn find_by_cosigner_commitment(
            &self,
            commitment: &str,
        ) -> Result<Vec<String>, String> {
            self.inner.find_by_cosigner_commitment(commitment).await
        }
        async fn set_pause(
            &self,
            account_id: &str,
            now: DateTime<Utc>,
            reason: &str,
        ) -> Result<PauseTransition, String> {
            let deleted = self.storage.get_delete_delta_proposal_calls().len();
            self.pauses
                .lock()
                .unwrap()
                .push((account_id.to_string(), reason.to_string(), deleted));
            self.inner.set_pause(account_id, now, reason).await
        }
        async fn clear_pause(&self, account_id: &str) -> Result<PauseTransition, String> {
            self.inner.clear_pause(account_id).await
        }
        async fn set_released_if_state(
            &self,
            account_id: &str,
            now: DateTime<Utc>,
            expected_state_commitment: &str,
            storage: &dyn crate::storage::StorageBackend,
        ) -> Result<ReleaseTransition, String> {
            self.inner
                .set_released_if_state(account_id, now, expected_state_commitment, storage)
                .await
        }
        async fn clear_released_if_state(
            &self,
            account_id: &str,
            expected_state_commitment: &str,
            storage: &dyn crate::storage::StorageBackend,
        ) -> Result<ClearTransition, String> {
            self.inner
                .clear_released_if_state(account_id, expected_state_commitment, storage)
                .await
        }
        async fn count_release_sweep_accounts(&self) -> Result<usize, String> {
            self.inner.count_release_sweep_accounts().await
        }
        async fn filter_release_sweep_ids(
            &self,
            ids: &[String],
        ) -> Result<ReleaseSweepMembership, String> {
            self.inner.filter_release_sweep_ids(ids).await
        }
    }

    /// A leader that counts acquisitions and releases. The first
    /// `refusals` acquisitions find the lease held elsewhere.
    #[derive(Default)]
    struct CountingLeader {
        refusals: Mutex<usize>,
        acquired: Mutex<usize>,
        released: Mutex<usize>,
    }

    impl CountingLeader {
        fn refusing(refusals: usize) -> Arc<Self> {
            Arc::new(Self {
                refusals: Mutex::new(refusals),
                ..Self::default()
            })
        }
        fn acquired(&self) -> usize {
            *self.acquired.lock().unwrap()
        }
        fn released(&self) -> usize {
            *self.released.lock().unwrap()
        }
    }

    #[async_trait]
    impl LeaderElector for CountingLeader {
        async fn try_acquire(&self, _ttl: Duration) -> crate::error::Result<Option<Lease>> {
            let mut refusals = self.refusals.lock().unwrap();
            if *refusals > 0 {
                *refusals -= 1;
                return Ok(None);
            }
            *self.acquired.lock().unwrap() += 1;
            Ok(Some(Lease {
                name: "canonicalization".to_string(),
                holder_id: "test".to_string(),
                fence_token: 1,
                expires_at: DateTime::<Utc>::MAX_UTC,
            }))
        }
        async fn renew(&self, _lease: &Lease, _ttl: Duration) -> crate::error::Result<bool> {
            Ok(true)
        }
        async fn verify_held(&self, _lease: &Lease) -> crate::error::Result<bool> {
            Ok(true)
        }
        async fn release(&self, _lease: Lease) -> crate::error::Result<()> {
            *self.released.lock().unwrap() += 1;
            Ok(())
        }
        fn supports_fencing(&self) -> bool {
            false
        }
    }

    /// Serves one prepared plan per account and records what it was given.
    #[derive(Clone, Default)]
    struct FakePlanner {
        plans: Arc<Mutex<HashMap<String, AccountPlan>>>,
        seen: Arc<Mutex<Vec<AccountInputs>>>,
    }

    impl FakePlanner {
        fn with(self, account_id: &str, plan: AccountPlan) -> Self {
            self.plans
                .lock()
                .unwrap()
                .insert(account_id.to_string(), plan);
            self
        }
        fn seen(&self) -> Vec<AccountInputs> {
            self.seen.lock().unwrap().clone()
        }
        fn planner(
            &self,
        ) -> impl Fn(&dyn NetworkClient, AccountInputs) -> AccountPlan + Send + Sync + 'static
        {
            let plans = self.plans.clone();
            let seen = self.seen.clone();
            move |_client: &dyn NetworkClient, inputs: AccountInputs| {
                let plan = plans
                    .lock()
                    .unwrap()
                    .remove(&inputs.metadata.account_id)
                    .expect("the test prepared a plan for every planned account");
                seen.lock().unwrap().push(inputs);
                plan
            }
        }
    }

    struct Harness {
        storage: MockStorageBackend,
        metadata: RecordingMetadata,
        network: MockNetworkClient,
        state: AppState,
        dir: tempfile::TempDir,
    }

    impl Harness {
        fn new(
            storage: MockStorageBackend,
            metadata: MockMetadataStore,
            network: MockNetworkClient,
        ) -> Self {
            let metadata = RecordingMetadata {
                inner: metadata,
                storage: storage.clone(),
                pauses: Arc::default(),
            };
            let state = create_test_app_state_with_mocks(
                Arc::new(storage.clone()),
                Arc::new(network.clone()),
                Arc::new(metadata.clone()),
            );
            Self {
                storage,
                metadata,
                network,
                state,
                dir: tempfile::tempdir().expect("temp dir"),
            }
        }

        fn config(&self, mode: SelfHealMode) -> SelfHealConfig {
            SelfHealConfig {
                mode,
                recovery_file: self.recovery_file(),
                max_concurrent_accounts: 1,
            }
        }

        fn recovery_file(&self) -> std::path::PathBuf {
            self.dir.path().join("recovery.json")
        }

        async fn run(
            &self,
            mode: SelfHealMode,
            planner: &FakePlanner,
        ) -> Result<SelfHealTotals, String> {
            run_with_planner(
                &self.state,
                Arc::new(crate::coordination::AlwaysLeader::new(
                    "canonicalization",
                    "test",
                )),
                &self.config(mode),
                planner.planner(),
            )
            .await
        }

        fn report(&self) -> serde_json::Value {
            let body = std::fs::read(self.recovery_file()).expect("the recovery file exists");
            serde_json::from_slice(&body).expect("the recovery file is JSON")
        }

        fn assert_no_storage_write(&self) {
            assert!(self.storage.get_apply_account_heal_calls().is_empty());
            assert!(self.storage.get_delete_delta_proposal_calls().is_empty());
            assert!(self.storage.get_delete_delta_calls().is_empty());
            assert!(self.storage.get_replace_state_calls().is_empty());
            assert!(self.storage.get_submit_state_calls().is_empty());
            assert!(self.metadata.pauses().is_empty());
            assert!(
                self.metadata
                    .inner
                    .set_released_calls
                    .lock()
                    .unwrap()
                    .is_empty()
            );
        }
    }

    fn miden_meta(account_id: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec!["0xattacker".to_string()],
            },
            network_config: NetworkConfig::miden_default(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    fn stored_state(account_id: &str, commitment: &str) -> StateObject {
        StateObject {
            account_id: account_id.to_string(),
            state_json: serde_json::json!({ "account": "blob" }),
            commitment: commitment.to_string(),
            nonce: Some(3),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            auth_scheme: "falcon".to_string(),
        }
    }

    fn delta(account_id: &str, nonce: u64, status: DeltaStatus) -> DeltaObject {
        DeltaObject {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: format!("0xprev{nonce}"),
            new_commitment: Some(format!("0xnew{nonce}")),
            status,
            ..DeltaObject::default()
        }
    }

    fn pending_proposal(account_id: &str, nonce: u64) -> DeltaObject {
        delta(
            account_id,
            nonce,
            DeltaStatus::pending("2026-01-01T00:00:00Z".to_string(), "0xp".to_string()),
        )
    }

    fn heal_plan(account_id: &str, guardian: Option<String>) -> AccountPlan {
        AccountPlan::Heal(Box::new(HealPlan {
            write: AccountHealWrite {
                account_id: account_id.to_string(),
                expected_state_commitment: FORGED.to_string(),
                state: stored_state(account_id, ON_CHAIN),
                auth: Auth::MidenFalconRpo {
                    cosigner_commitments: vec!["0xtrue".to_string()],
                },
                has_pending_candidate: false,
                delta_writes: vec![HealDeltaWrite::Delete { nonce: 4 }],
                proposal_writes: Vec::new(),
                now: "2026-01-03T00:00:00Z".to_string(),
            },
            guardian_commitment: guardian,
            true_signers: vec!["0xtrue".to_string()],
            actions: vec![super::super::types::RowAction {
                row: RowKind::Delta,
                id: "4".to_string(),
                verdict: RowVerdict::Delete,
                reason: "not signed by the true signers".to_string(),
            }],
        }))
    }

    fn unrecovered(account_id: &str, reason: RecoveryReason, pause: bool) -> AccountPlan {
        let mut entry = rpc_error_entry(&miden_meta(account_id), &stored_state(account_id, FORGED));
        entry.reason = reason;
        entry.detail = None;
        entry.chain = ChainInfo {
            status: "mismatch".to_string(),
            commitment: Some(ON_CHAIN.to_string()),
        };
        entry.pause = pause;
        AccountPlan::Unrecovered(Box::new(entry))
    }

    fn mismatched_account(
        account_id: &str,
    ) -> (MockStorageBackend, MockMetadataStore, MockNetworkClient) {
        let storage =
            MockStorageBackend::new().with_pull_state(Ok(stored_state(account_id, FORGED)));
        let metadata = MockMetadataStore::new().with_list_paged(Ok(vec![miden_meta(account_id)]));
        let network =
            MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: ON_CHAIN.to_string(),
            }));
        (storage, metadata, network)
    }

    #[tokio::test]
    async fn off_does_nothing() {
        let (storage, metadata, network) = mismatched_account(ACCOUNT);
        let harness = Harness::new(storage, metadata, network);
        let leader = CountingLeader::refusing(0);
        let totals = run_with_planner(
            &harness.state,
            leader.clone(),
            &harness.config(SelfHealMode::Off),
            FakePlanner::default().planner(),
        )
        .await
        .expect("off succeeds");

        assert_eq!(totals, SelfHealTotals::default());
        assert_eq!(leader.acquired(), 0, "off never takes the lease");
        assert!(harness.network.get_verify_commitment_calls().is_empty());
        assert!(!harness.recovery_file().exists(), "off writes no file");
        harness.assert_no_storage_write();
    }

    #[tokio::test(start_paused = true)]
    async fn the_run_waits_for_the_lease_and_releases_it() {
        let harness = Harness::new(
            MockStorageBackend::new(),
            MockMetadataStore::new(),
            MockNetworkClient::new(),
        );
        let leader = CountingLeader::refusing(2);
        run_with_planner(
            &harness.state,
            leader.clone(),
            &harness.config(SelfHealMode::DryRun),
            FakePlanner::default().planner(),
        )
        .await
        .expect("the run succeeds once it holds the lease");

        assert_eq!(leader.acquired(), 1);
        assert_eq!(leader.released(), 1);
    }

    #[tokio::test]
    async fn healthy_accounts_make_one_chain_read_and_no_write() {
        let storage = MockStorageBackend::new()
            .with_pull_state(Ok(stored_state("0xa", "0xca")))
            .with_pull_state(Ok(stored_state("0xb", "0xcb")));
        let metadata = MockMetadataStore::new()
            .with_list_paged(Ok(vec![miden_meta("0xa"), miden_meta("0xb")]));
        let harness = Harness::new(storage, metadata, MockNetworkClient::new());
        let planner = FakePlanner::default();

        let totals = harness
            .run(SelfHealMode::Apply, &planner)
            .await
            .expect("apply succeeds");

        assert_eq!(totals.accounts, 2);
        assert_eq!(totals.healthy, 2);
        assert_eq!(harness.network.get_verify_commitment_calls().len(), 2);
        assert!(
            harness
                .network
                .get_verify_commitment_modes()
                .iter()
                .all(|mode| *mode == RpcReadMode::Configured)
        );
        assert!(
            planner.seen().is_empty(),
            "a healthy account is not planned"
        );
        assert!(
            harness
                .storage
                .get_pull_all_delta_proposals_calls()
                .is_empty()
        );
        harness.assert_no_storage_write();
        assert_eq!(harness.report()["accounts"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn dry_run_writes_the_file_and_never_writes_storage() {
        let (storage, metadata, network) = mismatched_account(ACCOUNT);
        let storage = storage
            .with_pull_deltas_after(Ok(vec![delta(
                ACCOUNT,
                0,
                DeltaStatus::canonical("2026-01-01T00:00:00Z".to_string()),
            )]))
            .with_pull_all_delta_proposals(Ok(vec![pending_proposal(ACCOUNT, 4)]));
        let harness = Harness::new(storage, metadata, network);
        let planner = FakePlanner::default().with(
            ACCOUNT,
            unrecovered(ACCOUNT, RecoveryReason::ReplayShort, true),
        );

        let totals = harness
            .run(SelfHealMode::DryRun, &planner)
            .await
            .expect("dry run succeeds");

        assert_eq!(totals.not_recovered, 1);
        harness.assert_no_storage_write();
        let seen = planner.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].chain,
            ChainRead::Mismatch {
                commitment: ON_CHAIN.to_string()
            }
        );
        assert_eq!(seen[0].stored.commitment, FORGED);
        assert_eq!(
            seen[0].deltas.len(),
            1,
            "the nonce 0 row reaches the planner"
        );
        assert_eq!(seen[0].proposals.len(), 1);
        let report = harness.report();
        assert_eq!(report["dry_run"], true);
        assert_eq!(report["accounts"][0]["reason"], "replay_short");
    }

    #[tokio::test]
    async fn apply_heal_writes_the_plan() {
        let (storage, metadata, network) = mismatched_account(ACCOUNT);
        let harness = Harness::new(storage, metadata, network);
        let planner = FakePlanner::default().with(ACCOUNT, heal_plan(ACCOUNT, None));

        let totals = harness
            .run(SelfHealMode::Apply, &planner)
            .await
            .expect("apply succeeds");

        assert_eq!(totals.healed, 1);
        let writes = harness.storage.get_apply_account_heal_calls();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].account_id, ACCOUNT);
        assert_eq!(writes[0].expected_state_commitment, FORGED);
        assert_eq!(writes[0].state.commitment, ON_CHAIN);
        assert!(matches!(
            writes[0].delta_writes.as_slice(),
            [HealDeltaWrite::Delete { nonce: 4 }]
        ));
        assert!(
            harness
                .metadata
                .inner
                .set_released_calls
                .lock()
                .unwrap()
                .is_empty()
        );
        assert!(harness.metadata.pauses().is_empty());
        let report = harness.report();
        assert_eq!(report["dry_run"], false);
        assert_eq!(report["totals"]["healed"], 1);
        assert_eq!(report["accounts"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn apply_heal_releases_an_account_bound_to_another_guardian() {
        let (storage, metadata, network) = mismatched_account(ACCOUNT);
        let harness = Harness::new(storage, metadata, network);
        let planner =
            FakePlanner::default().with(ACCOUNT, heal_plan(ACCOUNT, Some("0xforeign".to_string())));

        harness
            .run(SelfHealMode::Apply, &planner)
            .await
            .expect("apply succeeds");

        assert_eq!(
            *harness.metadata.inner.set_released_calls.lock().unwrap(),
            vec![ACCOUNT.to_string()]
        );
        assert_eq!(
            *harness
                .metadata
                .inner
                .set_released_expected_states
                .lock()
                .unwrap(),
            vec![ON_CHAIN.to_string()],
            "the release is checked against the true state"
        );
    }

    #[tokio::test]
    async fn apply_heal_keeps_an_account_bound_to_this_guardian() {
        for scheme in [SignatureScheme::Falcon, SignatureScheme::Ecdsa] {
            let (storage, metadata, network) = mismatched_account(ACCOUNT);
            let harness = Harness::new(storage, metadata, network);
            let own = harness.state.ack.commitment(&scheme).to_uppercase();
            let planner = FakePlanner::default().with(ACCOUNT, heal_plan(ACCOUNT, Some(own)));

            harness
                .run(SelfHealMode::Apply, &planner)
                .await
                .expect("apply succeeds");

            assert!(
                harness
                    .metadata
                    .inner
                    .set_released_calls
                    .lock()
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn apply_heal_counts_a_moved_state_as_not_recovered() {
        let (storage, metadata, network) = mismatched_account(ACCOUNT);
        let storage = storage.with_apply_account_heal(Ok(HealWriteOutcome::StateMoved {
            current: "0xother".to_string(),
        }));
        let harness = Harness::new(storage, metadata, network);
        let planner =
            FakePlanner::default().with(ACCOUNT, heal_plan(ACCOUNT, Some("0xforeign".to_string())));

        let totals = harness
            .run(SelfHealMode::Apply, &planner)
            .await
            .expect("a moved state does not stop the run");

        assert_eq!(totals.healed, 0);
        assert_eq!(totals.not_recovered, 1);
        assert!(
            harness
                .metadata
                .inner
                .set_released_calls
                .lock()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn apply_unrecovered_deletes_pending_proposals_then_pauses() {
        let (storage, metadata, network) = mismatched_account(ACCOUNT);
        let proposals = vec![pending_proposal(ACCOUNT, 4), pending_proposal(ACCOUNT, 5)];
        let storage = storage
            .with_pull_all_delta_proposals(Ok(proposals.clone()))
            .with_pull_all_delta_proposals(Ok(proposals));
        let harness = Harness::new(storage, metadata, network);
        let planner = FakePlanner::default().with(
            ACCOUNT,
            unrecovered(ACCOUNT, RecoveryReason::AmbiguousHistory, true),
        );

        let totals = harness
            .run(SelfHealMode::Apply, &planner)
            .await
            .expect("apply succeeds");

        assert_eq!(totals.not_recovered, 1);
        assert_eq!(
            harness.storage.get_delete_delta_proposal_calls(),
            vec![
                (ACCOUNT.to_string(), "0xnew4".to_string()),
                (ACCOUNT.to_string(), "0xnew5".to_string()),
            ]
        );
        assert_eq!(
            harness.metadata.pauses(),
            vec![(
                ACCOUNT.to_string(),
                "self-heal: ambiguous_history".to_string(),
                2
            )],
            "the pause comes after both deletes"
        );
        assert!(harness.storage.get_apply_account_heal_calls().is_empty());
        assert_eq!(
            harness.report()["accounts"][0]["reason"],
            "ambiguous_history"
        );
    }

    #[tokio::test]
    async fn apply_unrecovered_respects_pause_false_and_release() {
        let (storage, metadata, network) = mismatched_account(ACCOUNT);
        let harness = Harness::new(storage, metadata, network);
        let AccountPlan::Unrecovered(mut entry) =
            unrecovered(ACCOUNT, RecoveryReason::LeftThisGuardian, false)
        else {
            unreachable!("unrecovered builds an unrecovered plan")
        };
        entry.release = true;
        let planner = FakePlanner::default().with(ACCOUNT, AccountPlan::Unrecovered(entry));

        harness
            .run(SelfHealMode::Apply, &planner)
            .await
            .expect("apply succeeds");

        assert!(
            harness.metadata.pauses().is_empty(),
            "pause = false is kept"
        );
        assert!(
            harness.storage.get_delete_delta_proposal_calls().is_empty(),
            "the proposals of an account that is not paused are kept"
        );
        assert_eq!(
            *harness
                .metadata
                .inner
                .set_released_expected_states
                .lock()
                .unwrap(),
            vec![FORGED.to_string()],
            "the release is checked against the stored state"
        );
    }

    #[tokio::test]
    async fn rpc_error_stops_apply() {
        let (storage, metadata, _) = mismatched_account(ACCOUNT);
        let network = MockNetworkClient::new().with_verify_commitment(Err("node down".to_string()));
        let harness = Harness::new(storage, metadata, network);

        let error = harness
            .run(SelfHealMode::Apply, &FakePlanner::default())
            .await
            .expect_err("apply refuses to go on without the chain");

        assert!(error.contains("node down"), "{error}");
        harness.assert_no_storage_write();
        assert!(!harness.recovery_file().exists());
    }

    #[tokio::test]
    async fn rpc_error_is_recorded_in_dry_run() {
        let (storage, metadata, _) = mismatched_account(ACCOUNT);
        let network = MockNetworkClient::new().with_verify_commitment(Err("node down".to_string()));
        let harness = Harness::new(storage, metadata, network);
        let planner = FakePlanner::default();

        let totals = harness
            .run(SelfHealMode::DryRun, &planner)
            .await
            .expect("dry run goes on");

        assert_eq!(totals.not_recovered, 1);
        assert!(planner.seen().is_empty());
        harness.assert_no_storage_write();
        let entry = &harness.report()["accounts"][0];
        assert_eq!(entry["reason"], "rpc_error");
        assert_eq!(entry["chain"]["status"], "unreadable");
        assert_eq!(entry["stored"]["commitment"], FORGED);
        assert!(entry.get("pause").is_none(), "pause is not serialized");
    }

    #[tokio::test]
    async fn released_and_non_miden_accounts_are_skipped() {
        let mut released = miden_meta("0xreleased");
        released.released_at = Some(Utc::now());
        let mut evm = miden_meta("0xevm");
        evm.network_config = NetworkConfig::Evm {
            chain_id: 1,
            account_address: "0x01".to_string(),
            multisig_validator_address: "0x02".to_string(),
        };
        let storage = MockStorageBackend::new().with_pull_state(Ok(stored_state("0xa", "0xca")));
        let metadata =
            MockMetadataStore::new().with_list_paged(Ok(vec![released, evm, miden_meta("0xa")]));
        let harness = Harness::new(storage, metadata, MockNetworkClient::new());

        let totals = harness
            .run(SelfHealMode::Apply, &FakePlanner::default())
            .await
            .expect("apply succeeds");

        assert_eq!(totals.accounts, 3);
        assert_eq!(totals.skipped, 2);
        assert_eq!(totals.healthy, 1);
        assert_eq!(
            harness.network.get_verify_commitment_calls(),
            vec![("0xa".to_string(), "0xca".to_string())]
        );
    }

    #[tokio::test]
    async fn a_second_run_after_apply_is_a_no_op() {
        let (storage, metadata, network) = mismatched_account(ACCOUNT);
        let storage = storage.with_pull_state(Ok(stored_state(ACCOUNT, ON_CHAIN)));
        let metadata = metadata.with_list_paged(Ok(vec![miden_meta(ACCOUNT)]));
        let network = MockNetworkClient {
            verify_commitment_responses: Arc::new(Mutex::new(vec![
                Ok(StateVerification::Match),
                Ok(StateVerification::Mismatch {
                    on_chain: ON_CHAIN.to_string(),
                }),
            ])),
            ..network
        };
        let harness = Harness::new(storage, metadata, network);
        let planner = FakePlanner::default().with(ACCOUNT, heal_plan(ACCOUNT, None));

        let first = harness.run(SelfHealMode::Apply, &planner).await.unwrap();
        let second = harness.run(SelfHealMode::Apply, &planner).await.unwrap();

        assert_eq!(first.healed, 1);
        assert_eq!(second.healthy, 1);
        assert_eq!(second.healed, 0);
        assert_eq!(harness.storage.get_apply_account_heal_calls().len(), 1);
    }

    #[tokio::test]
    async fn the_file_sorts_accounts_and_carries_the_totals() {
        let ids = ["0xc", "0xa", "0xb"];
        let mut storage = MockStorageBackend::new();
        let mut network = MockNetworkClient::new();
        let mut planner = FakePlanner::default();
        for id in ids {
            storage = storage.with_pull_state(Ok(stored_state(id, FORGED)));
            network = network.with_verify_commitment(Ok(StateVerification::Absent));
            planner = planner.with(id, unrecovered(id, RecoveryReason::NoBase, true));
        }
        let metadata = MockMetadataStore::new()
            .with_list_paged(Ok(ids.iter().map(|id| miden_meta(id)).collect()));
        let harness = Harness::new(storage, metadata, network);

        harness
            .run(SelfHealMode::DryRun, &planner)
            .await
            .expect("dry run succeeds");

        let report = harness.report();
        assert_eq!(report["schema"], 1);
        assert_eq!(report["dry_run"], true);
        assert!(report["generated_at"].is_string());
        assert!(report["network"].is_string());
        assert_eq!(report["totals"]["accounts"], 3);
        assert_eq!(report["totals"]["not_recovered"], 3);
        let listed: Vec<&str> = report["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["account_id"].as_str().unwrap())
            .collect();
        assert_eq!(listed, vec!["0xa", "0xb", "0xc"]);
        assert!(
            planner
                .seen()
                .iter()
                .all(|inputs| inputs.chain == ChainRead::Absent)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_file_replaces_an_old_one_atomically_with_owner_only_access() {
        use std::os::unix::fs::PermissionsExt;

        let harness = Harness::new(
            MockStorageBackend::new(),
            MockMetadataStore::new(),
            MockNetworkClient::new(),
        );
        std::fs::write(harness.recovery_file(), b"old content").unwrap();
        std::fs::set_permissions(
            harness.recovery_file(),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        harness
            .run(SelfHealMode::DryRun, &FakePlanner::default())
            .await
            .expect("dry run succeeds");

        let mode = std::fs::metadata(harness.recovery_file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(harness.report()["schema"], 1);
        let names: Vec<String> = std::fs::read_dir(harness.dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["recovery.json".to_string()],
            "no temp file is left"
        );
    }

    #[test]
    fn labels_match_the_serialized_names() {
        for reason in [
            RecoveryReason::NoBase,
            RecoveryReason::CreationDeltaPending,
            RecoveryReason::ReplayShort,
            RecoveryReason::LeftThisGuardian,
            RecoveryReason::AmbiguousHistory,
            RecoveryReason::UndecodableRow,
            RecoveryReason::RpcError,
        ] {
            assert_eq!(serde_json::json!(reason), reason_label(reason));
        }
        for verdict in [
            RowVerdict::Canonical,
            RowVerdict::Keep,
            RowVerdict::KeepWithFewerSignatures,
            RowVerdict::Retain,
            RowVerdict::Delete,
        ] {
            assert_eq!(serde_json::json!(verdict), verdict_label(verdict));
        }
        for kind in [RowKind::Delta, RowKind::Proposal] {
            assert_eq!(serde_json::json!(kind), row_kind_label(kind));
        }
    }
}
