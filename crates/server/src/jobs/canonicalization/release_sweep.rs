//! Periodic chain-driven release detection (issue #434).
//!
//! The push-path hook (`services::release_on_switch`) releases an account
//! when a `SwitchGuardian` delta canonicalizes here. Switches that never
//! reach the push path — the offline switch path, a failed best-effort
//! push, a client older than the push mechanism, a switch executed while
//! this server was unreachable — left the old guardian serving the account
//! as active forever: stale reads, refused-then-retried mutations, and a
//! capacity slot held for nothing. This dedicated
//! [`ProcessingMode::ReleaseSweep`] pass closes that gap by asking the
//! chain directly.
//!
//! Per visited account, in order and cheapest first:
//!
//! 1. One commitment probe against the stored state. While the chain sits
//!    at the stored commitment (or has no state for the account yet) the
//!    stored state *is* the on-chain state, so its guardian key — this
//!    server's, as `/configure` validated — is the on-chain one too:
//!    nothing to do, no second read.
//! 2. Only when the chain moved past the stored base: one storage read of
//!    the guardian public key map from the account's published on-chain
//!    state. Private accounts do not publish storage; the chain holds a
//!    bare commitment for them and the sweep records that it cannot tell.
//! 3. A foreign guardian key must be observed on
//!    `release_sweep_confirmations` consecutive visits before the account
//!    is released through the same path the push hook uses; accounts with
//!    an open streak are re-probed on the very next pass instead of
//!    waiting a full rotation.
//!
//! The pass visits at most `release_sweep_page_size` accounts under a
//! rotation cursor over `account_id`, never runs while a full pass is
//! due, and skips accounts with a candidate in flight (the push path owns
//! those). It deliberately ignores `paused_at`, like the other passes:
//! pause gates client mutations, the sweep records chain truth.

use super::*;
use crate::metadata::AccountMetadata;
use crate::metrics::labels::ReleaseSweepOutcome;
use crate::network::{OnChainGuardianBinding, RpcReadMode};
use crate::services::release_on_switch::{
    ReleaseEvidence, ReleaseWrite, own_guardian_commitment, release_switched_account,
};

/// Cross-pass state for the release sweep: the rotation cursor over
/// `account_id`, and the per-account confirmation streaks of accounts
/// observed with a foreign guardian key but not yet released. Both are
/// replica-local; a failover restarts the rotation and the streaks,
/// which only delays a release by the confirmation count.
#[derive(Default)]
pub(in crate::jobs::canonicalization) struct ReleaseSweepState {
    cursor: Mutex<Option<String>>,
    /// account_id → (observed foreign guardian commitment, consecutive
    /// observations). Cleared on any observation that is not that key.
    streaks: Mutex<BTreeMap<String, (String, u32)>>,
}

impl ReleaseSweepState {
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

    /// Accounts with an open confirmation streak, in id order.
    fn pending_confirmations(&self) -> Vec<String> {
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

    #[cfg(test)]
    pub(super) fn streak(&self, account_id: &str) -> Option<u32> {
        self.streaks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(account_id)
            .map(|(_, count)| *count)
    }
}

fn record_sweep_outcome(outcome: ReleaseSweepOutcome) {
    metrics::counter!(
        crate::metrics::names::RELEASE_SWEEP_ACCOUNTS_TOTAL,
        crate::metrics::names::LABEL_OUTCOME => outcome.as_str()
    )
    .increment(1);
}

impl DeltasProcessorBase {
    /// One release sweep pass: accounts awaiting confirmation first, then
    /// one page of unreleased accounts after the rotation cursor. Bounded
    /// by the pass deadline the worker derives from the next full-pass
    /// tick, so the sweep can never delay candidate processing.
    pub(super) async fn process_release_sweep_pass(&self) -> Result<PassSummary> {
        let started = Instant::now();

        let page_size = self.release_sweep_page_size.max(1);
        let cursor = self.release_sweep_state.cursor();
        let mut page = self.list_sweep_page(cursor.as_deref(), page_size).await?;
        // An empty page past a cursor means the previous page ended
        // exactly at the last id: wrap around now rather than idling a
        // whole pass.
        let mut wrapped = false;
        if page.is_empty() && cursor.is_some() {
            wrapped = true;
            page = self.list_sweep_page(None, page_size).await?;
        }
        // The pass is already past its deadline (or cancelled): leave
        // the cursor where it is so the page is visited next time
        // instead of being skipped for a whole rotation.
        if self.admission_closed() {
            return Ok(self.pass_summary(0, 0));
        }
        // A short page means the walk reached the end of the id space;
        // the next pass wraps around to the beginning.
        wrapped |= page.len() < page_size as usize;
        self.release_sweep_state.set_cursor(if wrapped {
            None
        } else {
            page.last().map(|m| m.account_id.clone())
        });

        // Accounts with an open confirmation streak are re-probed every
        // pass so a release takes `confirmations` passes, not rotations.
        // They are resolved by id because they may sit anywhere in the
        // rotation; one that no longer exists or was released elsewhere
        // simply drops its streak.
        let mut visit: Vec<AccountMetadata> = Vec::with_capacity(page.len());
        let in_page: std::collections::HashSet<&str> =
            page.iter().map(|m| m.account_id.as_str()).collect();
        for account_id in self.release_sweep_state.pending_confirmations() {
            if in_page.contains(account_id.as_str()) {
                continue;
            }
            match self.state.metadata.get(&account_id).await {
                Ok(Some(metadata)) if metadata.released_at.is_none() => visit.push(metadata),
                Ok(_) => self.release_sweep_state.clear_streak(&account_id),
                Err(e) => tracing::warn!(
                    account_id = %account_id,
                    error = %e,
                    "Failed to reload an account awaiting release confirmation; \
                     retrying next pass"
                ),
            }
        }
        visit.extend(page);

        let accounts = visit.len();
        let failed_accounts = futures::stream::iter(visit)
            .map(|metadata| async move { self.sweep_account_absorbing(&metadata).await })
            .buffer_unordered(self.max_concurrent_accounts.max(1))
            .fold(0, |failed, account_failed| async move {
                failed + usize::from(account_failed)
            })
            .await;

        tracing::debug!(
            accounts,
            failed_accounts,
            wrapped,
            deadline_reached = self.pass_deadline_reached(),
            duration_seconds = started.elapsed().as_secs_f64(),
            "Release sweep pass completed"
        );

        Ok(self.pass_summary(accounts, failed_accounts))
    }

    async fn list_sweep_page(
        &self,
        after: Option<&str>,
        page_size: u32,
    ) -> Result<Vec<AccountMetadata>> {
        self.state
            .metadata
            .list_release_sweep_page(after, page_size)
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to list sweepable accounts: {e}"))
            })
    }

    async fn sweep_account_absorbing(&self, metadata: &AccountMetadata) -> bool {
        if self.admission_closed() {
            return false;
        }
        match self.sweep_account(metadata).await {
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

    /// Sweep one account: the cheap commitment probe, then the storage
    /// read only when the chain moved past the stored base.
    async fn sweep_account(&self, metadata: &AccountMetadata) -> Result<()> {
        let account_id = metadata.account_id.as_str();

        // The store already filters these; the checks stay for rows
        // reloaded by id (pending confirmations) and for defense in
        // depth. EVM accounts have no on-chain guardian binding; a row
        // released between listing and visiting has nothing left to
        // decide.
        if metadata.network_config.is_evm() || metadata.released_at.is_some() {
            self.release_sweep_state.clear_streak(account_id);
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
                self.release_sweep_state.clear_streak(account_id);
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
                record_sweep_outcome(ReleaseSweepOutcome::ProbeFailed);
                Ok(())
            }
            Ok(StateVerification::Mismatch { on_chain }) => {
                self.sweep_diverged_account(metadata, &stored, &on_chain)
                    .await
            }
        }
    }

    /// The chain moved past the stored base. Either a transaction this
    /// server acknowledged landed without the store catching up (the
    /// guardian key is still ours — issue #345 territory, not a switch),
    /// or the one transaction the account can execute without this
    /// server's signature ran: a guardian key rotation. Only published
    /// storage can tell the two apart.
    async fn sweep_diverged_account(
        &self,
        metadata: &AccountMetadata,
        stored: &StateObject,
        on_chain: &str,
    ) -> Result<()> {
        let account_id = metadata.account_id.as_str();

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
                record_sweep_outcome(ReleaseSweepOutcome::ProbeFailed);
                return Ok(());
            }
            Ok(OnChainGuardianBinding::Opaque) => {
                tracing::info!(
                    event = "release_sweep_deferred",
                    reason = "storage_opaque",
                    account_id = %account_id,
                    stored_commitment = %stored.commitment,
                    on_chain = %on_chain,
                    "Chain moved past the stored base but the account does not \
                     publish its storage; the guardian binding cannot be verified \
                     from chain"
                );
                record_sweep_outcome(ReleaseSweepOutcome::StorageOpaque);
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
            self.release_sweep_state.clear_streak(account_id);
            tracing::debug!(
                event = "release_sweep_deferred",
                reason = "chain_at_stored_base",
                account_id = %account_id,
                "Storage read shows the chain at the stored base; probe reads disagreed"
            );
            return Ok(());
        }

        let own_commitment = own_guardian_commitment(&self.state, metadata);
        let new_guardian_commitment = match guardian_commitment {
            None => {
                self.release_sweep_state.clear_streak(account_id);
                tracing::info!(
                    event = "release_sweep_deferred",
                    reason = "no_guardian_binding",
                    account_id = %account_id,
                    on_chain = %on_chain_commitment,
                    "Published storage carries no guardian binding; absence is not a switch"
                );
                record_sweep_outcome(ReleaseSweepOutcome::NoBinding);
                return Ok(());
            }
            Some(commitment) if commitment == own_commitment => {
                self.release_sweep_state.clear_streak(account_id);
                tracing::info!(
                    event = "release_sweep_deferred",
                    reason = "guardian_still_bound",
                    account_id = %account_id,
                    stored_commitment = %stored.commitment,
                    on_chain = %on_chain_commitment,
                    "Chain moved past the stored base but the account is still \
                     bound to this guardian (stored state lagging the chain)"
                );
                record_sweep_outcome(ReleaseSweepOutcome::StillBound);
                return Ok(());
            }
            Some(commitment) => commitment,
        };

        let streak = self
            .release_sweep_state
            .record_foreign_observation(account_id, &new_guardian_commitment);
        if streak < self.release_sweep_confirmations {
            tracing::info!(
                event = "release_sweep_confirming",
                account_id = %account_id,
                new_guardian_commitment = %new_guardian_commitment,
                observations = streak,
                confirmations = self.release_sweep_confirmations,
                "On-chain guardian key differs from this server's; awaiting confirmation"
            );
            record_sweep_outcome(ReleaseSweepOutcome::Confirming);
            return Ok(());
        }

        // Re-check the stored base right before writing: a `/configure`
        // that re-onboarded the account meanwhile replaced the stored
        // state (and cleared any release), so the evidence gathered
        // against the old base no longer applies. The next pass
        // re-evaluates from the new base.
        let current = self
            .state
            .storage
            .pull_state(account_id)
            .await
            .map_err(|e| GuardianError::StorageError(format!("Failed to re-read state: {e}")))?;
        if current.commitment != stored.commitment {
            self.release_sweep_state.clear_streak(account_id);
            tracing::info!(
                event = "release_sweep_deferred",
                reason = "stored_base_moved",
                account_id = %account_id,
                "Stored state moved during the sweep; re-evaluating from the new base next pass"
            );
            return Ok(());
        }

        match release_switched_account(
            &self.state,
            metadata,
            &new_guardian_commitment,
            ReleaseEvidence::ChainSweep {
                on_chain_commitment: &on_chain_commitment,
                stored_commitment: &stored.commitment,
            },
        )
        .await
        {
            ReleaseWrite::Released => {
                self.release_sweep_state.clear_streak(account_id);
                tracing::info!(
                    event = "release_sweep_released",
                    account_id = %account_id,
                    new_guardian_commitment = %new_guardian_commitment,
                    on_chain = %on_chain_commitment,
                    "Released an account whose guardian switch never reached the push path"
                );
                record_sweep_outcome(ReleaseSweepOutcome::Released);
                Ok(())
            }
            ReleaseWrite::AlreadyReleased => {
                self.release_sweep_state.clear_streak(account_id);
                Ok(())
            }
            // The streak is kept: the observation stands, only the write
            // failed, so the next pass retries the release directly.
            ReleaseWrite::Failed => Err(GuardianError::StorageError(format!(
                "Failed to persist release for switched account {account_id}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::kinds;
    use crate::metadata::auth::Auth;
    use crate::metadata::{AccountMetadata, NetworkConfig};
    use crate::network::OnChainGuardianBinding;
    use crate::testing::helpers::{CapturingAuditor, create_test_app_state_with_mocks};
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use guardian_shared::SignatureScheme;

    const STORED: &str = "0xstored_commitment";
    const ON_CHAIN: &str = "0xchain_commitment";
    const FOREIGN: &str = "0xforeign_guardian";

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

    struct Harness {
        processor: DeltasProcessor,
        network: Arc<MockNetworkClient>,
        metadata: Arc<MockMetadataStore>,
        auditor: CapturingAuditor,
    }

    /// Single-process sweep processor over the given mocks. The mock
    /// queues pop LIFO, so callers queue responses in reverse order of
    /// use.
    fn harness(
        storage: MockStorageBackend,
        network: MockNetworkClient,
        metadata: MockMetadataStore,
        config: CanonicalizationConfig,
    ) -> Harness {
        let network = Arc::new(network);
        let metadata = Arc::new(metadata);
        let auditor = CapturingAuditor::new();
        let mut state =
            create_test_app_state_with_mocks(Arc::new(storage), network.clone(), metadata.clone());
        state.auditor = Arc::new(auditor.clone());
        let processor = DeltasProcessor::new_with_mode(state, config, ProcessingMode::ReleaseSweep);
        Harness {
            processor,
            network,
            metadata,
            auditor,
        }
    }

    fn released_ids(metadata: &MockMetadataStore) -> Vec<String> {
        metadata.set_released_calls.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn releases_a_public_account_after_the_configured_confirmations() {
        // Pass 1: the account comes from the page, the chain shows a
        // foreign guardian key -> confirming. Pass 2: the page is empty,
        // the account is re-visited as a pending confirmation via
        // `get`, the same key is observed again -> released, audited
        // with the chain-sweep evidence.
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
            MockMetadataStore::new()
                // pass 2 (LIFO: queued first, popped last)
                .with_get(Ok(Some(miden_meta(account_id))))
                // pass 1
                .with_list_release_sweep_page(Ok(vec![miden_meta(account_id)])),
            CanonicalizationConfig::new(10, 18).with_release_sweep_confirmations(2),
        );

        let first = h.processor.process_all_accounts().await.expect("pass 1");
        assert_eq!(first.accounts, 1);
        assert_eq!(first.failed_accounts, 0);
        assert!(
            released_ids(&h.metadata).is_empty(),
            "one observation is not enough"
        );
        assert_eq!(
            h.processor.base.release_sweep_state.streak(account_id),
            Some(1)
        );
        assert!(h.auditor.snapshot().is_empty());

        let second = h.processor.process_all_accounts().await.expect("pass 2");
        assert_eq!(second.accounts, 1, "the pending confirmation is re-visited");
        assert_eq!(released_ids(&h.metadata), vec![account_id.to_string()]);
        assert_eq!(
            h.processor.base.release_sweep_state.streak(account_id),
            None
        );
        assert_eq!(
            h.network.get_fetch_on_chain_guardian_binding_calls(),
            vec![account_id.to_string(), account_id.to_string()]
        );

        let events = h.auditor.snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action_kind, kinds::ACCOUNTS_RELEASE);
        assert_eq!(events[0].target_account_id.as_deref(), Some(account_id));
        assert_eq!(events[0].payload["detected_by"], "chain_sweep");
        assert_eq!(events[0].payload["new_guardian_commitment"], FOREIGN);
        assert_eq!(events[0].payload["on_chain_commitment"], ON_CHAIN);
        assert_eq!(events[0].payload["stored_commitment"], STORED);
    }

    #[tokio::test]
    async fn a_still_bound_lagging_account_is_never_released() {
        // The chain moved past the stored base but the published
        // guardian key is this server's own: a stored-state lag (#345),
        // not a switch. No release, no streak.
        let account_id = "0xacc";
        let h = harness(
            MockStorageBackend::new().with_pull_state(Ok(stored_state(account_id, STORED))),
            MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: ON_CHAIN.into(),
            })),
            MockMetadataStore::new().with_list_release_sweep_page(Ok(vec![miden_meta(account_id)])),
            CanonicalizationConfig::new(10, 18).with_release_sweep_confirmations(1),
        );
        let own = h
            .processor
            .base
            .state
            .ack
            .commitment(&SignatureScheme::Falcon);
        h.network
            .fetch_on_chain_guardian_binding_responses
            .lock()
            .unwrap()
            .push(Ok(visible(Some(&own))));

        let summary = h.processor.process_all_accounts().await.expect("pass");
        assert_eq!(summary.failed_accounts, 0);
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(
            h.processor.base.release_sweep_state.streak(account_id),
            None
        );
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
                MockMetadataStore::new()
                    .with_list_release_sweep_page(Ok(vec![miden_meta(account_id)])),
                CanonicalizationConfig::new(10, 18).with_release_sweep_confirmations(1),
            );

            let summary = h.processor.process_all_accounts().await.expect("pass");
            assert_eq!(summary.failed_accounts, 0);
            assert!(
                released_ids(&h.metadata).is_empty(),
                "{binding:?} must never release"
            );
            assert_eq!(
                h.processor.base.release_sweep_state.streak(account_id),
                None
            );
        }
    }

    #[tokio::test]
    async fn chain_at_the_stored_base_skips_the_storage_read_and_clears_the_streak() {
        for verification in [StateVerification::Match, StateVerification::Absent] {
            let account_id = "0xacc";
            let h = harness(
                MockStorageBackend::new().with_pull_state(Ok(stored_state(account_id, STORED))),
                MockNetworkClient::new()
                    .with_verify_commitment(Ok(verification.clone()))
                    .with_fetch_on_chain_guardian_binding(Ok(visible(Some(FOREIGN)))),
                MockMetadataStore::new()
                    .with_list_release_sweep_page(Ok(vec![miden_meta(account_id)])),
                CanonicalizationConfig::new(10, 18).with_release_sweep_confirmations(1),
            );
            // A stale streak from an earlier pass must not survive a
            // chain-at-base observation.
            h.processor
                .base
                .release_sweep_state
                .record_foreign_observation(account_id, FOREIGN);

            h.processor.process_all_accounts().await.expect("pass");
            assert_eq!(
                h.network.get_verify_commitment_calls(),
                vec![(account_id.to_string(), STORED.to_string())]
            );
            assert!(
                h.network
                    .get_fetch_on_chain_guardian_binding_calls()
                    .is_empty(),
                "{verification:?}: no storage read while the chain is at the stored base"
            );
            assert!(released_ids(&h.metadata).is_empty());
            assert_eq!(
                h.processor.base.release_sweep_state.streak(account_id),
                None
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
                // pass 2: the storage read fails
                .with_verify_commitment(Ok(StateVerification::Mismatch {
                    on_chain: ON_CHAIN.into(),
                }))
                .with_fetch_on_chain_guardian_binding(Err("rpc down".into()))
                // pass 1: the commitment probe fails
                .with_verify_commitment(Err("rpc down".into())),
            MockMetadataStore::new()
                .with_get(Ok(Some(miden_meta(account_id))))
                .with_list_release_sweep_page(Ok(vec![miden_meta(account_id)])),
            CanonicalizationConfig::new(10, 18).with_release_sweep_confirmations(2),
        );
        h.processor
            .base
            .release_sweep_state
            .record_foreign_observation(account_id, FOREIGN);

        let first = h.processor.process_all_accounts().await.expect("pass 1");
        assert_eq!(
            first.failed_accounts, 0,
            "a probe failure is a deferral, not an error"
        );
        assert_eq!(
            h.processor.base.release_sweep_state.streak(account_id),
            Some(1)
        );

        let second = h.processor.process_all_accounts().await.expect("pass 2");
        assert_eq!(second.failed_accounts, 0);
        assert_eq!(
            h.processor.base.release_sweep_state.streak(account_id),
            Some(1)
        );
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
            MockMetadataStore::new().with_list_release_sweep_page(Ok(vec![pending, evm, released])),
            CanonicalizationConfig::new(10, 18).with_release_sweep_confirmations(1),
        );

        let summary = h.processor.process_all_accounts().await.expect("pass");
        assert_eq!(summary.accounts, 3);
        assert_eq!(summary.failed_accounts, 0);
        assert!(h.network.get_verify_commitment_calls().is_empty());
        assert!(released_ids(&h.metadata).is_empty());
    }

    #[tokio::test]
    async fn a_stored_base_that_moved_before_the_write_defers_the_release() {
        // Between gathering the evidence and writing, `/configure`
        // re-onboarded the account (new stored state): the release is
        // skipped and re-evaluated from the new base next pass.
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
            MockMetadataStore::new().with_list_release_sweep_page(Ok(vec![miden_meta(account_id)])),
            CanonicalizationConfig::new(10, 18).with_release_sweep_confirmations(1),
        );

        let summary = h.processor.process_all_accounts().await.expect("pass");
        assert_eq!(summary.failed_accounts, 0);
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(
            h.processor.base.release_sweep_state.streak(account_id),
            None
        );
        assert!(h.auditor.snapshot().is_empty());
    }

    #[tokio::test]
    async fn disagreeing_reads_are_not_an_observation() {
        // The commitment probe says the chain moved, the storage read
        // says it sits at the stored base: a lagging node somewhere. No
        // release, no streak, even though the storage read reports a
        // foreign key.
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
            MockMetadataStore::new().with_list_release_sweep_page(Ok(vec![miden_meta(account_id)])),
            CanonicalizationConfig::new(10, 18).with_release_sweep_confirmations(1),
        );

        h.processor.process_all_accounts().await.expect("pass");
        assert!(released_ids(&h.metadata).is_empty());
        assert_eq!(
            h.processor.base.release_sweep_state.streak(account_id),
            None
        );
    }

    #[tokio::test]
    async fn rotation_cursor_pages_through_the_fleet_and_wraps() {
        // Page size 2 over five accounts: pass 1 visits two and parks the
        // cursor after the second; pass 2 resumes after it with a full
        // page (cursor after 0xd); pass 3 resumes after 0xd, gets a
        // short page, and resets the cursor; pass 4 starts over.
        let h = harness(
            MockStorageBackend::new(),
            MockNetworkClient::new(),
            MockMetadataStore::new()
                // pass 4 (popped last)
                .with_list_release_sweep_page(Ok(vec![miden_meta("0xa"), miden_meta("0xb")]))
                // pass 3
                .with_list_release_sweep_page(Ok(vec![miden_meta("0xe")]))
                // pass 2
                .with_list_release_sweep_page(Ok(vec![miden_meta("0xc"), miden_meta("0xd")]))
                // pass 1
                .with_list_release_sweep_page(Ok(vec![miden_meta("0xa"), miden_meta("0xb")])),
            CanonicalizationConfig::new(10, 18)
                .with_release_sweep_page_size(2)
                .with_release_sweep_confirmations(1),
        );

        for _ in 0..4 {
            h.processor.process_all_accounts().await.expect("pass");
        }
        assert_eq!(
            h.metadata.get_list_release_sweep_page_calls(),
            vec![
                (None, 2),
                (Some("0xb".to_string()), 2),
                (Some("0xd".to_string()), 2),
                (None, 2),
            ]
        );
    }

    #[tokio::test]
    async fn an_exact_multiple_of_the_page_size_wraps_without_an_idle_pass() {
        // Four accounts, page size 2: after the second full page the
        // next pass finds an empty page past the cursor and re-lists
        // from the start in the same pass instead of idling.
        let h = harness(
            MockStorageBackend::new(),
            MockNetworkClient::new(),
            MockMetadataStore::new()
                // pass 3, second listing after the empty page (popped last)
                .with_list_release_sweep_page(Ok(vec![miden_meta("0xa"), miden_meta("0xb")]))
                // pass 3, first listing: empty past 0xd
                .with_list_release_sweep_page(Ok(vec![]))
                // pass 2
                .with_list_release_sweep_page(Ok(vec![miden_meta("0xc"), miden_meta("0xd")]))
                // pass 1
                .with_list_release_sweep_page(Ok(vec![miden_meta("0xa"), miden_meta("0xb")])),
            CanonicalizationConfig::new(10, 18)
                .with_release_sweep_page_size(2)
                .with_release_sweep_confirmations(1),
        );

        let mut visited = 0;
        for _ in 0..3 {
            visited += h
                .processor
                .process_all_accounts()
                .await
                .expect("pass")
                .accounts;
        }
        assert_eq!(visited, 6, "the wrap-around pass still visits a full page");
        assert_eq!(
            h.metadata.get_list_release_sweep_page_calls(),
            vec![
                (None, 2),
                (Some("0xb".to_string()), 2),
                (Some("0xd".to_string()), 2),
                (None, 2),
            ]
        );
    }

    #[tokio::test]
    async fn a_pass_past_its_deadline_leaves_the_cursor_untouched() {
        let pass = PassLease::single_process();
        let metadata = Arc::new(
            MockMetadataStore::new()
                .with_list_release_sweep_page(Ok(vec![miden_meta("0xa"), miden_meta("0xb")])),
        );
        let state = create_test_app_state_with_mocks(
            Arc::new(MockStorageBackend::new()),
            Arc::new(MockNetworkClient::new()),
            metadata.clone(),
        );
        let processor = DeltasProcessor::with_controls(
            state,
            CanonicalizationConfig::new(10, 18).with_release_sweep_page_size(2),
            pass.leader,
            pass.lease,
            pass.cancel,
            ProcessingMode::ReleaseSweep,
            PassControls {
                fast_state: Arc::new(FastPromotionState::default()),
                reconcile_state: Arc::new(ReconcileState::default()),
                release_sweep_state: Arc::new(ReleaseSweepState::default()),
                deadline: Some(Instant::now()),
                reconcile_backoff: false,
            },
        );

        let summary = processor.process_all_accounts().await.expect("pass");
        assert_eq!(summary.accounts, 0);
        assert_eq!(
            processor.base.release_sweep_state.cursor(),
            None,
            "the page is re-listed next pass instead of being skipped for a rotation"
        );
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
            MockMetadataStore::new()
                .with_list_release_sweep_page(Ok(vec![miden_meta(account_id)]))
                .with_set_released(Err("db down".into())),
            CanonicalizationConfig::new(10, 18).with_release_sweep_confirmations(1),
        );

        let summary = h.processor.process_all_accounts().await.expect("pass");
        assert_eq!(summary.failed_accounts, 1);
        assert_eq!(
            h.processor.base.release_sweep_state.streak(account_id),
            Some(1),
            "the observation stands; only the write is retried"
        );
        assert!(h.auditor.snapshot().is_empty());
    }

    #[test]
    fn foreign_observation_streak_counts_agreeing_keys_only() {
        let state = ReleaseSweepState::default();
        assert_eq!(state.record_foreign_observation("a", "0xb"), 1);
        assert_eq!(state.record_foreign_observation("a", "0xb"), 2);
        // A different foreign key restarts the count at one.
        assert_eq!(state.record_foreign_observation("a", "0xc"), 1);
        assert_eq!(state.record_foreign_observation("a", "0xc"), 2);
        assert_eq!(state.pending_confirmations(), vec!["a".to_string()]);

        state.clear_streak("a");
        assert!(state.pending_confirmations().is_empty());
        assert_eq!(state.streak("a"), None);
    }
}
