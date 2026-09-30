//! Release-on-guardian-switch detection (issue #305).
//!
//! Since #287 the multisig clients push a `SwitchGuardian` delta to the
//! **pre-switch** guardian, best-effort, so it canonicalizes there like
//! any other delta. This hook is the server-side reaction: after a
//! delta commits (optimistic mode) or canonicalizes (candidate mode),
//! compare the guardian public key commitment recorded in the resulting
//! account state against this server's own ack key. When they differ,
//! the account has verifiably switched to a different guardian — this
//! server's co-signatures are no longer accepted on-chain — so the
//! account transitions to the terminal `released` state: mutations are
//! refused by the pause/release chokepoint, reads keep working so the
//! wallet and operator can fetch final state, and only re-onboarding
//! through `/configure` (which re-validates the guardian binding)
//! reactivates it.
//!
//! Best-effort by design: a failure here is logged and never fails the
//! delta commit — the delta itself is valid and already persisted. In
//! candidate mode a mid-canonicalization failure leaves the delta a
//! candidate, so the worker retries and this hook runs again.
//!
//! The push path is not the only detector. A switch executed while this
//! server was unreachable, from a client that never pushes (the offline
//! switch path), or whose best-effort push failed, never arrives here;
//! the release sweep (`jobs::release_sweep`, issue #434) either finds the
//! post-state of a pending proposal or an unpromoted switch delta on
//! chain (at the head or in the account's transaction history) or reads
//! the guardian binding from a state the chain holds (published storage,
//! or this server's own stored state when the chain sits at it), and
//! drives the same transition through [`release_switched_account`] with
//! `ReleaseEvidence::ProposalMatch` / `RecoverableDelta` / `ChainSweep`.
//! It also retries a push-path release whose write failed, with the
//! `Delta` evidence this hook would have recorded.
//!
//! Every release is conditional on the stored state still being the one
//! the evidence was proved against (see
//! [`crate::metadata::MetadataStore::set_released_if_state`]): a
//! `/configure` that re-onboarded the account meanwhile voids it.

use serde_json::json;

use crate::audit::{AuditEvent, AuditOutcome, kinds};
use crate::metadata::{AccountMetadata, ReleaseTransition};
use crate::state::AppState;

/// `operator_identity` recorded on system-initiated release audit rows.
pub const SYSTEM_OPERATOR_IDENTITY: &str = "system";

/// How a guardian switch was observed, recorded on the release audit
/// row (`detected_by`) with the observation that proves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseEvidence<'a> {
    /// A `SwitchGuardian` delta committed / canonicalized on this
    /// server; the resulting state carries the new guardian key. The
    /// release sweep records the same evidence when that delta's own
    /// release write failed.
    Delta {
        delta_nonce: u64,
        new_commitment: &'a str,
    },
    /// The release sweep read the new guardian key from the account's
    /// published on-chain storage, at a chain state that had moved past
    /// this server's stored one.
    ChainSweep {
        on_chain_commitment: &'a str,
        stored_commitment: &'a str,
    },
    /// The release sweep found the post-state of a proposal pending on
    /// this server (its summary applied to the stored state) on chain:
    /// either the chain sits at it now, or the account's transaction
    /// history holds a transaction that ended at it (`switch_block_num`).
    /// An exact commitment proves that switch executed, and needs no
    /// published storage, so it covers private accounts.
    ProposalMatch {
        proposal_id: &'a str,
        on_chain_commitment: &'a str,
        stored_commitment: &'a str,
        switch_commitment: &'a str,
        switch_block_num: Option<u32>,
    },
    /// Like `ProposalMatch`, for a switch delta that did reach this
    /// server but was never promoted: canonicalization retained it (the
    /// chain had moved past its post-state) or the client abandoned it,
    /// and its proposal was deleted on the way.
    RecoverableDelta {
        delta_nonce: u64,
        on_chain_commitment: &'a str,
        stored_commitment: &'a str,
        switch_commitment: &'a str,
        switch_block_num: Option<u32>,
    },
}

impl ReleaseEvidence<'_> {
    /// The `detected_by` label of the audit row and the release logs.
    pub(crate) fn detected_by(&self) -> &'static str {
        match self {
            Self::Delta { .. } => "delta",
            Self::ChainSweep { .. } => "chain_sweep",
            Self::ProposalMatch { .. } => "proposal_match",
            Self::RecoverableDelta { .. } => "recoverable_delta",
        }
    }

    /// The stored state the evidence was proved against: the release is
    /// written only while the store still holds it.
    fn expected_state_commitment(&self) -> &str {
        match self {
            Self::Delta { new_commitment, .. } => new_commitment,
            Self::ChainSweep {
                stored_commitment, ..
            }
            | Self::ProposalMatch {
                stored_commitment, ..
            }
            | Self::RecoverableDelta {
                stored_commitment, ..
            } => stored_commitment,
        }
    }

    /// The `accounts.release` audit payload. Every variant carries
    /// `new_guardian_commitment` and `detected_by`; the delta variant
    /// keeps the original `{ delta_nonce, new_commitment }` keys.
    fn audit_payload(&self, new_guardian_commitment: &str) -> serde_json::Value {
        match self {
            Self::Delta {
                delta_nonce,
                new_commitment,
            } => json!({
                "new_guardian_commitment": new_guardian_commitment,
                "detected_by": self.detected_by(),
                "delta_nonce": delta_nonce,
                "new_commitment": new_commitment,
            }),
            Self::ChainSweep {
                on_chain_commitment,
                stored_commitment,
            } => json!({
                "new_guardian_commitment": new_guardian_commitment,
                "detected_by": self.detected_by(),
                "on_chain_commitment": on_chain_commitment,
                "stored_commitment": stored_commitment,
            }),
            Self::ProposalMatch {
                proposal_id,
                on_chain_commitment,
                stored_commitment,
                switch_commitment,
                switch_block_num,
            } => json!({
                "new_guardian_commitment": new_guardian_commitment,
                "detected_by": self.detected_by(),
                "proposal_id": proposal_id,
                "on_chain_commitment": on_chain_commitment,
                "stored_commitment": stored_commitment,
                "switch_commitment": switch_commitment,
                "switch_block_num": switch_block_num,
            }),
            Self::RecoverableDelta {
                delta_nonce,
                on_chain_commitment,
                stored_commitment,
                switch_commitment,
                switch_block_num,
            } => json!({
                "new_guardian_commitment": new_guardian_commitment,
                "detected_by": self.detected_by(),
                "delta_nonce": delta_nonce,
                "on_chain_commitment": on_chain_commitment,
                "stored_commitment": stored_commitment,
                "switch_commitment": switch_commitment,
                "switch_block_num": switch_block_num,
            }),
        }
    }
}

/// Outcome of [`release_switched_account`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseWrite {
    /// This call transitioned the account to `released` and audited it.
    Released,
    /// The account was already released (first-writer-wins); nothing
    /// was written or audited.
    AlreadyReleased,
    /// The stored state moved away from the one the evidence was proved
    /// against (a `/configure` re-onboarded the account): nothing was
    /// written or audited, and the evidence no longer applies.
    StateMoved,
    /// The transition could not be persisted; logged. The release sweep
    /// retries it: its next visit either re-proves the switch it was
    /// working on, or (after a push-path promotion) finds a stored state
    /// whose guardian key is not this server's, produced by a switch
    /// delta whose ack this server signed.
    Failed,
}

/// Release the account when `new_state_json` (the just-committed state)
/// carries a guardian public key commitment different from this
/// server's ack key. Infallible for callers: all failures are logged.
pub async fn release_if_guardian_switched(
    state: &AppState,
    metadata: &AccountMetadata,
    new_state_json: &serde_json::Value,
    delta_nonce: u64,
    new_commitment: &str,
) {
    // EVM accounts have no on-chain guardian binding to compare.
    if metadata.network_config.is_evm() {
        return;
    }

    let own_commitment = own_guardian_commitment(state, metadata);

    let extracted = {
        let client = &state.network_client;
        client.extract_guardian_commitment(new_state_json)
    };

    let new_guardian_commitment = match extracted {
        // `None` means the state carries no guardian binding at all
        // (e.g. guardian component absent). Absence is not evidence of
        // a switch — do nothing.
        Ok(Some(commitment)) if commitment != own_commitment => commitment,
        Ok(_) => return,
        Err(e) => {
            tracing::error!(
                account_id = %metadata.account_id,
                nonce = delta_nonce,
                error = %e,
                "Failed to inspect guardian commitment after delta commit; \
                 release-on-switch check skipped"
            );
            return;
        }
    };

    release_switched_account(
        state,
        metadata,
        &new_guardian_commitment,
        ReleaseEvidence::Delta {
            delta_nonce,
            new_commitment,
        },
    )
    .await;
}

/// This server's own guardian public key commitment for the account's
/// signature scheme — the value a bound account's guardian slot holds.
pub fn own_guardian_commitment(state: &AppState, metadata: &AccountMetadata) -> String {
    state.ack.commitment(&metadata.auth.scheme())
}

/// Transition the account to `released` because its guardian key is
/// verifiably `new_guardian_commitment` rather than this server's, and
/// audit the transition with the evidence. Shared by the push-path hook
/// and the release sweep so both detectors produce one lifecycle and
/// one audit shape. The write happens only while the stored state is
/// still the one the evidence was proved against, atomically with that
/// check, so a concurrent `/configure` re-onboarding never ends up
/// released. Infallible for callers: all failures are logged.
pub async fn release_switched_account(
    state: &AppState,
    metadata: &AccountMetadata,
    new_guardian_commitment: &str,
    evidence: ReleaseEvidence<'_>,
) -> ReleaseWrite {
    match state
        .metadata
        .set_released_if_state(
            &metadata.account_id,
            state.clock.now(),
            evidence.expected_state_commitment(),
            state.storage.as_ref(),
        )
        .await
    {
        Ok(ReleaseTransition::Released) => {
            tracing::warn!(
                account_id = %metadata.account_id,
                detected_by = evidence.detected_by(),
                evidence = ?evidence,
                new_guardian_commitment = %new_guardian_commitment,
                "Account switched to a different guardian; released \
                 (mutations refused until re-onboarded via /configure)"
            );
            state.auditor.record(AuditEvent {
                operator_identity: SYSTEM_OPERATOR_IDENTITY.to_string(),
                action_kind: kinds::ACCOUNTS_RELEASE,
                target_account_id: Some(metadata.account_id.clone()),
                payload: evidence.audit_payload(new_guardian_commitment),
                outcome: AuditOutcome::Success,
                error_code: None,
                client_ip: None,
            });
            ReleaseWrite::Released
        }
        // Already released — first-writer-wins, nothing to audit.
        Ok(ReleaseTransition::AlreadyReleased) => ReleaseWrite::AlreadyReleased,
        Ok(ReleaseTransition::StateMoved) => {
            tracing::info!(
                account_id = %metadata.account_id,
                detected_by = evidence.detected_by(),
                evidence = ?evidence,
                "Guardian switch evidence no longer applies: the stored state was \
                 replaced (a /configure re-onboarding or a promoted delta) before the \
                 release was written"
            );
            ReleaseWrite::StateMoved
        }
        Err(e) => {
            tracing::error!(
                account_id = %metadata.account_id,
                detected_by = evidence.detected_by(),
                evidence = ?evidence,
                error = %e,
                "Detected guardian switch but failed to persist released state"
            );
            ReleaseWrite::Failed
        }
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::ack::AckRegistry;
    use crate::audit::kinds;
    use crate::builder::clock::test::MockClock;
    use crate::metadata::{AccountMetadata, Auth, NetworkConfig};
    use crate::storage::filesystem::FilesystemService;
    use crate::testing::helpers::CapturingAuditor;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn miden_meta(account_id: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec!["0xc1".into()],
            },
            network_config: NetworkConfig::miden_default(),
            created_at: "2026-07-01T00:00:00Z".into(),
            updated_at: "2026-07-01T00:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    async fn state_with(
        network: MockNetworkClient,
        metadata: MockMetadataStore,
        auditor: CapturingAuditor,
    ) -> AppState {
        let dir = TempDir::new().expect("tempdir");
        let storage = FilesystemService::new(dir.path().to_path_buf())
            .await
            .expect("svc");
        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");
        AppState {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata),
            network_client: Arc::new(network),
            ack,
            canonicalization: None,
            release_sweep: None,
            clock: Arc::new(MockClock::default()),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            auditor: Arc::new(auditor),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    #[tokio::test]
    async fn releases_and_audits_when_guardian_differs() {
        let network = MockNetworkClient::new()
            .with_extract_guardian_commitment(Ok(Some("0xother_guardian".into())));
        let metadata_store = MockMetadataStore::new();
        let auditor = CapturingAuditor::new();
        let state = state_with(network, metadata_store.clone(), auditor.clone()).await;

        release_if_guardian_switched(
            &state,
            &miden_meta("acc-1"),
            &serde_json::json!({}),
            7,
            "0xnew_commitment",
        )
        .await;

        assert_eq!(
            metadata_store.set_released_calls.lock().unwrap().clone(),
            vec!["acc-1".to_string()],
            "guardian mismatch must persist the released transition"
        );
        let events = auditor.snapshot();
        assert_eq!(events.len(), 1, "release must emit exactly one audit event");
        assert_eq!(events[0].action_kind, kinds::ACCOUNTS_RELEASE);
        assert_eq!(events[0].operator_identity, SYSTEM_OPERATOR_IDENTITY);
        assert_eq!(events[0].target_account_id.as_deref(), Some("acc-1"));
        assert_eq!(
            events[0].payload["new_guardian_commitment"],
            "0xother_guardian"
        );
        assert_eq!(events[0].payload["delta_nonce"], 7);
        assert_eq!(events[0].payload["new_commitment"], "0xnew_commitment");
        assert_eq!(events[0].payload["detected_by"], "delta");
        assert_eq!(
            metadata_store
                .set_released_expected_states
                .lock()
                .unwrap()
                .clone(),
            vec!["0xnew_commitment".to_string()],
            "the push path proves the switch against the state it just stored"
        );
    }

    #[tokio::test]
    async fn a_state_replaced_before_the_write_releases_nothing() {
        // A /configure re-onboarded the account between the detection
        // and the write: the store refuses, and nothing is audited.
        let metadata_store = MockMetadataStore::new()
            .with_set_released(Ok(crate::metadata::ReleaseTransition::StateMoved));
        let auditor = CapturingAuditor::new();
        let state = state_with(
            MockNetworkClient::new(),
            metadata_store.clone(),
            auditor.clone(),
        )
        .await;

        let outcome = release_switched_account(
            &state,
            &miden_meta("acc-1"),
            "0xother_guardian",
            ReleaseEvidence::ChainSweep {
                on_chain_commitment: "0xchain",
                stored_commitment: "0xstored",
            },
        )
        .await;

        assert_eq!(outcome, ReleaseWrite::StateMoved);
        assert_eq!(
            metadata_store
                .set_released_expected_states
                .lock()
                .unwrap()
                .clone(),
            vec!["0xstored".to_string()]
        );
        assert!(auditor.snapshot().is_empty());
    }

    #[tokio::test]
    async fn a_failed_write_is_reported_and_not_audited() {
        let metadata_store = MockMetadataStore::new().with_set_released(Err("db down".into()));
        let auditor = CapturingAuditor::new();
        let state = state_with(
            MockNetworkClient::new(),
            metadata_store.clone(),
            auditor.clone(),
        )
        .await;

        let outcome = release_switched_account(
            &state,
            &miden_meta("acc-1"),
            "0xother_guardian",
            ReleaseEvidence::ChainSweep {
                on_chain_commitment: "0xchain",
                stored_commitment: "0xstored",
            },
        )
        .await;

        assert_eq!(outcome, ReleaseWrite::Failed);
        assert!(auditor.snapshot().is_empty());
    }

    #[tokio::test]
    async fn proposal_match_release_audits_where_the_switch_was_found() {
        let metadata_store = MockMetadataStore::new();
        let auditor = CapturingAuditor::new();
        let state = state_with(
            MockNetworkClient::new(),
            metadata_store.clone(),
            auditor.clone(),
        )
        .await;

        let outcome = release_switched_account(
            &state,
            &miden_meta("acc-1"),
            "0xother_guardian",
            ReleaseEvidence::ProposalMatch {
                proposal_id: "0xproposal",
                on_chain_commitment: "0xhead",
                stored_commitment: "0xstored",
                switch_commitment: "0xpost_switch",
                switch_block_num: Some(406_750),
            },
        )
        .await;

        assert_eq!(outcome, ReleaseWrite::Released);
        assert_eq!(
            metadata_store
                .set_released_expected_states
                .lock()
                .unwrap()
                .clone(),
            vec!["0xstored".to_string()]
        );
        let events = auditor.snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].payload,
            serde_json::json!({
                "new_guardian_commitment": "0xother_guardian",
                "detected_by": "proposal_match",
                "proposal_id": "0xproposal",
                "on_chain_commitment": "0xhead",
                "stored_commitment": "0xstored",
                "switch_commitment": "0xpost_switch",
                "switch_block_num": 406_750,
            })
        );
    }

    #[tokio::test]
    async fn chain_sweep_release_audits_the_chain_observation() {
        let metadata_store = MockMetadataStore::new();
        let auditor = CapturingAuditor::new();
        let state = state_with(
            MockNetworkClient::new(),
            metadata_store.clone(),
            auditor.clone(),
        )
        .await;

        let outcome = release_switched_account(
            &state,
            &miden_meta("acc-1"),
            "0xother_guardian",
            ReleaseEvidence::ChainSweep {
                on_chain_commitment: "0xchain",
                stored_commitment: "0xstored",
            },
        )
        .await;

        assert_eq!(outcome, ReleaseWrite::Released);
        assert_eq!(
            metadata_store.set_released_calls.lock().unwrap().clone(),
            vec!["acc-1".to_string()]
        );
        let events = auditor.snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action_kind, kinds::ACCOUNTS_RELEASE);
        assert_eq!(events[0].operator_identity, SYSTEM_OPERATOR_IDENTITY);
        assert_eq!(events[0].target_account_id.as_deref(), Some("acc-1"));
        assert_eq!(
            events[0].payload,
            serde_json::json!({
                "new_guardian_commitment": "0xother_guardian",
                "detected_by": "chain_sweep",
                "on_chain_commitment": "0xchain",
                "stored_commitment": "0xstored",
            })
        );
    }

    #[tokio::test]
    async fn no_release_when_guardian_is_this_server() {
        let network = MockNetworkClient::new();
        let network_handle = network.clone();
        let metadata_store = MockMetadataStore::new();
        let auditor = CapturingAuditor::new();
        let state = state_with(network, metadata_store.clone(), auditor.clone()).await;

        // The state's guardian key IS this server's ack key.
        let own = state
            .ack
            .commitment(&guardian_shared::SignatureScheme::Falcon);
        network_handle
            .extract_guardian_commitment_responses
            .lock()
            .unwrap()
            .push(Ok(Some(own)));

        release_if_guardian_switched(
            &state,
            &miden_meta("acc-1"),
            &serde_json::json!({}),
            7,
            "0xc",
        )
        .await;

        assert!(metadata_store.set_released_calls.lock().unwrap().is_empty());
        assert!(auditor.snapshot().is_empty());
    }

    #[tokio::test]
    async fn no_release_when_state_has_no_guardian_binding() {
        // Default mock response is Ok(None): no guardian slot visible.
        let network = MockNetworkClient::new();
        let metadata_store = MockMetadataStore::new();
        let auditor = CapturingAuditor::new();
        let state = state_with(network, metadata_store.clone(), auditor.clone()).await;

        release_if_guardian_switched(
            &state,
            &miden_meta("acc-1"),
            &serde_json::json!({}),
            7,
            "0xc",
        )
        .await;

        assert!(metadata_store.set_released_calls.lock().unwrap().is_empty());
        assert!(auditor.snapshot().is_empty());
    }

    #[tokio::test]
    async fn extraction_error_is_swallowed_without_release() {
        let network =
            MockNetworkClient::new().with_extract_guardian_commitment(Err("corrupt state".into()));
        let metadata_store = MockMetadataStore::new();
        let auditor = CapturingAuditor::new();
        let state = state_with(network, metadata_store.clone(), auditor.clone()).await;

        release_if_guardian_switched(
            &state,
            &miden_meta("acc-1"),
            &serde_json::json!({}),
            7,
            "0xc",
        )
        .await;

        assert!(metadata_store.set_released_calls.lock().unwrap().is_empty());
        assert!(auditor.snapshot().is_empty());
    }

    #[tokio::test]
    async fn evm_accounts_are_skipped() {
        // Even with a differing commitment queued, EVM accounts must
        // never release — there is no on-chain guardian binding.
        let network = MockNetworkClient::new()
            .with_extract_guardian_commitment(Ok(Some("0xother_guardian".into())));
        let metadata_store = MockMetadataStore::new();
        let auditor = CapturingAuditor::new();
        let state = state_with(network, metadata_store.clone(), auditor.clone()).await;

        let mut meta = miden_meta("evm:1:0xabc");
        meta.network_config = NetworkConfig::Evm {
            chain_id: 1,
            account_address: "0xabc".into(),
            multisig_validator_address: "0xdef".into(),
        };

        release_if_guardian_switched(&state, &meta, &serde_json::json!({}), 7, "0xc").await;

        assert!(metadata_store.set_released_calls.lock().unwrap().is_empty());
        assert!(auditor.snapshot().is_empty());
    }
}
