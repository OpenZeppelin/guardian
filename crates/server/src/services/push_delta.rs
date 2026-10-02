use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::services::account_status::ensure_account_active_metadata;
use crate::services::delta_commit::{CommitContext, DeltaCommitStrategy};
use crate::services::resolve_account;
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct PushDeltaParams {
    pub delta: DeltaObject,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct PushDeltaResult {
    pub delta: DeltaObject,
}

#[tracing::instrument(
    level = "info",
    skip(state, params),
    fields(account_id = %params.delta.account_id)
)]
pub async fn push_delta(state: &AppState, params: PushDeltaParams) -> Result<PushDeltaResult> {
    tracing::debug!("Pushing delta");

    let resolved = resolve_account(state, &params.delta.account_id, &params.credentials).await?;
    ensure_account_active_metadata(&resolved.metadata)?;
    if resolved.metadata.network_config.is_evm() {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "push_delta".to_string(),
        });
    }

    let current_state = resolved
        .storage
        .pull_state(&params.delta.account_id)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %params.delta.account_id,
                error = %e,
                "Failed to fetch account state in push_delta"
            );
            GuardianError::StorageError(format!("Failed to fetch account state: {e}"))
        })?;

    if state.canonicalization.is_some()
        && let Some(active) = resolved
            .storage
            .load_active_execution(&params.delta.account_id)
            .await
            .map_err(GuardianError::StorageError)?
    {
        return Err(GuardianError::ExecutionConflict {
            blocking_proposal_id: active.reservation.proposal_id,
        });
    }

    // Check for pending candidates before accepting new delta
    let has_pending = resolved
        .storage
        .has_pending_candidate(&params.delta.account_id)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %params.delta.account_id,
                error = %e,
                "Failed to check deltas in push_delta"
            );
            GuardianError::StorageError(format!("Failed to check deltas: {e}"))
        })?;

    if has_pending {
        return Err(GuardianError::ConflictPendingDelta);
    }

    if params.delta.prev_commitment != current_state.commitment {
        return Err(GuardianError::CommitmentMismatch {
            expected: current_state.commitment.clone(),
            actual: params.delta.prev_commitment.clone(),
        });
    }

    let scheme = resolved.metadata.auth.scheme();
    let acknowledged = crate::services::ack_delta_internal::acknowledge_delta(
        state,
        &scheme,
        &current_state,
        &params.delta,
    )
    .await?;
    let mut result_delta = acknowledged.delta;
    let applied = acknowledged.applied;

    let now = state.clock.now_rfc3339();
    let commit_strategy = DeltaCommitStrategy::from_app_state(state);
    commit_strategy
        .commit(
            CommitContext {
                state,
                resolved: &resolved,
                current_state: &current_state,
                now,
            },
            &mut result_delta,
            applied,
        )
        .await?;
    // Caveat: `lookup_matching_proposal_payload` swallows storage
    // errors to `None` (non-fatal by design), so under storage faults
    // a proposal commit can be labeled `direct`. Acceptable skew — the
    // underlying fault is visible via
    // storage_operations_total{outcome="error"}.
    let kind = if acknowledged.matched_proposal {
        crate::metrics::labels::DeltaKind::ProposalCommit
    } else {
        crate::metrics::labels::DeltaKind::Direct
    };
    metrics::counter!(
        crate::metrics::names::DELTAS_SUBMITTED_TOTAL,
        crate::metrics::names::LABEL_KIND => kind.as_str()
    )
    .increment(1);

    Ok(PushDeltaResult {
        delta: result_delta,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::testing::helpers::create_test_app_state_with_mocks;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use chrono::TimeZone;
    use std::sync::Arc;

    fn paused_metadata(account_id: &str, cosigner_commitment: String) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![cosigner_commitment],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-01T00:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: Some(
                chrono::Utc
                    .with_ymd_and_hms(2026, 5, 19, 14, 30, 0)
                    .unwrap(),
            ),
            paused_reason: Some("compliance".to_string()),
            released_at: None,
        }
    }

    /// Pause-gate guard: `push_delta` MUST reject before touching
    /// storage or the network — but only AFTER authentication
    /// succeeds, so unauthenticated probes cannot leak pause state.
    #[tokio::test]
    async fn paused_account_rejected_before_side_effects() {
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let storage = MockStorageBackend::new();
        let network = MockNetworkClient::new();
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(paused_metadata(&account_id, signer_commitment))));

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(network.clone()),
            Arc::new(metadata.clone()),
        );

        let params = PushDeltaParams {
            delta: DeltaObject {
                account_id: account_id.clone(),
                ..Default::default()
            },
            credentials: Credentials::signature(signer_pubkey, signer_signature, signer_timestamp),
        };

        let err = push_delta(&state, params)
            .await
            .expect_err("paused account must be rejected");
        assert!(
            matches!(err, GuardianError::AccountPaused { ref paused_reason, .. }
                if paused_reason.as_deref() == Some("compliance")),
            "unexpected error: {err:?}"
        );

        assert!(
            storage.get_submit_delta_calls().is_empty(),
            "no delta should be submitted when the account is paused"
        );
    }

    /// Push-time metadata pipeline: decode TransactionSummary, look up
    /// the matching proposal, lift its `proposal_type` into the typed
    /// `DeltaMetadata`, persist on the candidate row, and verify the
    /// dashboard projection surfaces it.
    #[tokio::test]
    async fn push_delta_persists_metadata_with_proposal_from_matching_proposal_lookup() {
        use crate::delta_object::DeltaStatus;
        use crate::delta_summary::DashboardDeltaCategory;
        use crate::state_object::StateObject;
        use crate::testing::helpers::create_test_delta_payload;

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let candidate_payload = create_test_delta_payload(&account_id);

        let proposal_wrapper = serde_json::json!({
            "tx_summary": create_test_delta_payload(&account_id),
            "metadata": {
                "proposal_type": "consume_notes",
                "note_ids": ["0xnote0000000000000000000000000001"],
                "consume_notes_metadata_version": 2,
                "consume_notes_notes": ["c29tZWJhc2U2NA=="],
                "required_signatures": 2,
            },
            "signatures": [],
        });
        let prev_commitment = "0xprev".to_string();
        let stored_state = StateObject {
            account_id: account_id.clone(),
            state_json: serde_json::json!({}),
            commitment: prev_commitment.clone(),
            nonce: None,
            created_at: "2026-05-25T08:00:00Z".into(),
            updated_at: "2026-05-25T08:00:00Z".into(),
            auth_scheme: String::new(),
        };
        let storage = MockStorageBackend::new()
            .with_pull_state(Ok(stored_state))
            .with_pull_deltas_after(Ok(Vec::new()))
            .with_pull_delta_proposal(Ok(DeltaObject {
                account_id: account_id.clone(),
                nonce: 1,
                prev_commitment: prev_commitment.clone(),
                new_commitment: None,
                delta_payload: proposal_wrapper,
                ack_sig: String::new(),
                ack_pubkey: String::new(),
                ack_scheme: String::new(),
                status: DeltaStatus::Pending {
                    timestamp: "2026-05-25T07:59:00Z".to_string(),
                    proposer_id: "0xproposer".to_string(),
                    cosigner_sigs: vec![],
                },
                metadata: None,
            }))
            .with_submit_state(Ok(()))
            .with_submit_delta(Ok(()));

        let network = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_verify_delta(Ok(()))
            .with_apply_delta(Ok((
                serde_json::json!({"new_state": true}),
                "0xnew_commitment".to_string(),
            )));

        let metadata = MockMetadataStore::new().with_get(Ok(Some(AccountMetadata {
            account_id: account_id.clone(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![signer_commitment],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-01T00:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        })));

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(network.clone()),
            Arc::new(metadata.clone()),
        );

        let params = PushDeltaParams {
            delta: DeltaObject {
                account_id: account_id.clone(),
                nonce: 1,
                prev_commitment: prev_commitment.clone(),
                new_commitment: None,
                delta_payload: candidate_payload,
                ack_sig: String::new(),
                ack_pubkey: String::new(),
                ack_scheme: String::new(),
                status: DeltaStatus::default(),
                metadata: None,
            },
            credentials: Credentials::signature(signer_pubkey, signer_signature, signer_timestamp),
        };

        let result = push_delta(&state, params)
            .await
            .expect("push succeeds with valid inputs");

        let persisted = storage
            .get_submit_delta_calls()
            .into_iter()
            .last()
            .expect("submit_delta was called");
        let lifted = persisted
            .metadata
            .as_ref()
            .expect("metadata persisted on candidate row");
        assert_eq!(lifted.category, DashboardDeltaCategory::NoteConsumption);
        let proposal = lifted
            .proposal
            .as_ref()
            .expect("proposal block lifted from matching delta_proposals row");
        assert_eq!(proposal.proposal_type, "consume_notes");
        assert_eq!(proposal.required_signatures, Some(2));
        assert_eq!(proposal.note_ids.len(), 1);
        assert_eq!(proposal.consume_notes_metadata_version, Some(2));

        assert!(result.delta.metadata.is_some());
        assert_eq!(result.delta.proposal_type(), Some("consume_notes"));
    }

    /// Direct push path: no matching proposal in storage. Candidate is
    /// still persisted with derived metadata; `proposal` absent. This
    /// is the regression guard for "push_delta does not require
    /// push_delta_proposal".
    #[tokio::test]
    async fn direct_push_delta_succeeds_without_existing_proposal() {
        use crate::delta_object::DeltaStatus;
        use crate::delta_summary::DashboardDeltaCategory;
        use crate::state_object::StateObject;
        use crate::testing::helpers::create_test_delta_payload;

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let candidate_payload = create_test_delta_payload(&account_id);
        let prev_commitment = "0xprev".to_string();
        let stored_state = StateObject {
            account_id: account_id.clone(),
            state_json: serde_json::json!({}),
            commitment: prev_commitment.clone(),
            nonce: None,
            created_at: "2026-05-25T08:00:00Z".into(),
            updated_at: "2026-05-25T08:00:00Z".into(),
            auth_scheme: String::new(),
        };
        let storage = MockStorageBackend::new()
            .with_pull_state(Ok(stored_state))
            .with_pull_deltas_after(Ok(Vec::new()))
            .with_pull_delta_proposal(Err("no matching proposal".to_string()))
            .with_submit_state(Ok(()))
            .with_submit_delta(Ok(()));

        let network = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_verify_delta(Ok(()))
            .with_apply_delta(Ok((
                serde_json::json!({"new_state": true}),
                "0xnew_commitment".to_string(),
            )));

        let metadata = MockMetadataStore::new().with_get(Ok(Some(AccountMetadata {
            account_id: account_id.clone(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![signer_commitment],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-01T00:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        })));

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(network.clone()),
            Arc::new(metadata.clone()),
        );

        let params = PushDeltaParams {
            delta: DeltaObject {
                account_id: account_id.clone(),
                nonce: 1,
                prev_commitment: prev_commitment.clone(),
                new_commitment: None,
                delta_payload: candidate_payload,
                ack_sig: String::new(),
                ack_pubkey: String::new(),
                ack_scheme: String::new(),
                status: DeltaStatus::default(),
                metadata: None,
            },
            credentials: Credentials::signature(signer_pubkey, signer_signature, signer_timestamp),
        };

        push_delta(&state, params)
            .await
            .expect("push succeeds with valid inputs");

        let persisted = storage
            .get_submit_delta_calls()
            .into_iter()
            .last()
            .expect("submit_delta was called");
        let lifted = persisted
            .metadata
            .as_ref()
            .expect("metadata persisted from on-chain summary alone");
        assert_eq!(
            lifted.category,
            DashboardDeltaCategory::AccountStorageChange
        );
        assert!(
            lifted.proposal.is_none(),
            "no matching proposal → no proposal block"
        );
        assert!(persisted.proposal_type().is_none());
        assert!(
            storage.get_submit_delta_proposal_calls().is_empty(),
            "direct push must not create a delta proposal"
        );
        assert!(
            storage.get_update_delta_proposal_calls().is_empty(),
            "direct push must not update a delta proposal"
        );
        assert!(
            storage.get_delete_delta_proposal_calls().is_empty(),
            "direct push must not delete a delta proposal"
        );
        assert!(
            !storage.get_pull_delta_proposal_calls().is_empty(),
            "direct push may probe for matching proposal metadata, but misses are non-fatal"
        );
    }

    #[tokio::test]
    async fn paused_account_returns_auth_error_for_unauthenticated_caller() {
        let storage = MockStorageBackend::new();
        let network = MockNetworkClient::new();
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(paused_metadata("acc-paused", "0xc1".into()))));

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(network.clone()),
            Arc::new(metadata.clone()),
        );

        let params = PushDeltaParams {
            delta: DeltaObject {
                account_id: "acc-paused".to_string(),
                ..Default::default()
            },
            credentials: Credentials::signature(String::new(), String::new(), 0),
        };

        let err = push_delta(&state, params)
            .await
            .expect_err("unauthenticated paused account must be rejected with auth error");
        assert!(
            matches!(err, GuardianError::AuthenticationFailed(_)),
            "unauthenticated caller must not learn pause state; got: {err:?}"
        );
    }
}
