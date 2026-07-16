use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::error::GuardianError;
use crate::services::ResolvedAccount;
use crate::state::AppState;
use crate::state_object::StateObject;
use crate::storage::CandidateSubmission;
use tracing::{debug, error, info, warn};

pub struct CommitContext<'a> {
    pub state: &'a AppState,
    pub resolved: &'a ResolvedAccount,
    pub current_state: &'a StateObject,
    pub now: String,
}

#[derive(Clone)]
pub enum DeltaCommitStrategy {
    Candidate,
    Optimistic,
}

impl DeltaCommitStrategy {
    pub fn from_app_state(state: &AppState) -> Self {
        if state.canonicalization.is_some() {
            Self::Candidate
        } else {
            Self::Optimistic
        }
    }

    pub async fn commit(
        &self,
        ctx: CommitContext<'_>,
        delta: &mut DeltaObject,
        new_state_json: serde_json::Value,
        new_commitment: &str,
    ) -> Result<(), GuardianError> {
        match self {
            DeltaCommitStrategy::Candidate => {
                delta.status = DeltaStatus::candidate(ctx.now.clone());
                // One storage write covers the candidate row and the
                // pending-candidate flag: a failure between the two can
                // otherwise leave a candidate the worker never selects
                // while new submissions stay rejected. A Conflict is the
                // race-proof form of the pre-commit pending-candidate
                // gate: the losing side of two concurrent submissions
                // gets the same 409 it would have gotten arriving late.
                let outcome = ctx
                    .resolved
                    .storage
                    .submit_candidate(ctx.state.metadata.as_ref(), delta, &ctx.now)
                    .await
                    .map_err(|e| {
                        error!(
                            account_id = %delta.account_id,
                            nonce = delta.nonce,
                            error = %e,
                            "Failed to submit candidate delta"
                        );
                        GuardianError::StorageError(format!("Failed to submit delta: {e}"))
                    })?;
                match outcome {
                    CandidateSubmission::Submitted => Ok(()),
                    CandidateSubmission::Conflict => {
                        warn!(
                            account_id = %delta.account_id,
                            nonce = delta.nonce,
                            "Candidate submission lost the commit race; rejecting as pending-delta conflict"
                        );
                        Err(GuardianError::ConflictPendingDelta)
                    }
                    CandidateSubmission::CommitmentMismatch { expected } => {
                        Err(GuardianError::CommitmentMismatch {
                            expected,
                            actual: delta.prev_commitment.clone(),
                        })
                    }
                }
            }
            DeltaCommitStrategy::Optimistic => {
                delta.status = DeltaStatus::canonical(ctx.now.clone());

                let new_state = StateObject {
                    account_id: delta.account_id.clone(),
                    commitment: new_commitment.to_string(),
                    state_json: new_state_json,
                    created_at: ctx.current_state.created_at.clone(),
                    updated_at: ctx.now.clone(),
                    auth_scheme: String::new(),
                };

                ctx.resolved
                    .storage
                    .submit_state(&new_state)
                    .await
                    .map_err(|e| {
                        error!(
                            account_id = %delta.account_id,
                            error = %e,
                            "Failed to update state in optimistic mode"
                        );
                        GuardianError::StorageError(format!("Failed to update state: {e}"))
                    })?;

                ctx.resolved
                    .storage
                    .submit_delta(delta)
                    .await
                    .map_err(|e| {
                        error!(
                            account_id = %delta.account_id,
                            nonce = delta.nonce,
                            error = %e,
                            "Failed to submit canonical delta in optimistic mode"
                        );
                        GuardianError::StorageError(format!("Failed to submit delta: {e}"))
                    })?;

                // Delete matching proposal now that delta is canonical
                let proposal_id = {
                    let client = &ctx.state.network_client;
                    client
                        .delta_proposal_id(&delta.account_id, delta.nonce, &delta.delta_payload)
                        .ok()
                };

                if let Some(ref id) = proposal_id {
                    match ctx
                        .resolved
                        .storage
                        .pull_delta_proposal(&delta.account_id, id)
                        .await
                    {
                        Ok(_existing_proposal) => {
                            info!(
                                account_id = %delta.account_id,
                                proposal_id = %id,
                                "Deleting matching proposal as delta is now canonical"
                            );
                            // Finalization is the canonical delta + matching
                            // proposal, not the cleanup delete succeeding —
                            // count it before attempting the delete. (This
                            // Optimistic-mode emit and the Candidate-mode one
                            // in jobs/canonicalization/processor.rs are
                            // mutually exclusive per deployment, not a
                            // double-count.)
                            metrics::counter!(
                                crate::metrics::names::PROPOSALS_TOTAL,
                                crate::metrics::names::LABEL_EVENT =>
                                    crate::metrics::labels::ProposalEvent::Finalized.as_str()
                            )
                            .increment(1);
                            if let Err(e) = ctx
                                .resolved
                                .storage
                                .delete_delta_proposal(&delta.account_id, id)
                                .await
                            {
                                warn!(
                                    account_id = %delta.account_id,
                                    proposal_id = %id,
                                    error = %e,
                                    "Failed to delete proposal, but continuing"
                                );
                            }
                        }
                        Err(e) => {
                            debug!(
                                account_id = %delta.account_id,
                                proposal_id = %id,
                                error = %e,
                                "No matching proposal to finalize after canonical delta \
                                 (absent or unreadable); skipping cleanup"
                            );
                        }
                    }
                }

                // Issue #305: if this delta moved the account's guardian
                // key away from this server (a SwitchGuardian pushed to
                // the pre-switch guardian), release the account. In
                // optimistic mode this runs at commit time — the same
                // trust level as every other optimistic commit.
                crate::services::release_on_switch::release_if_guardian_switched(
                    ctx.state,
                    &ctx.resolved.metadata,
                    &new_state.state_json,
                    delta.nonce,
                    &new_state.commitment,
                )
                .await;

                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta_object::DeltaStatus;
    use crate::metadata::AccountMetadata;
    use crate::testing::helpers::create_test_app_state_with_mocks;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use std::sync::Arc;

    fn create_test_delta() -> DeltaObject {
        DeltaObject {
            account_id: "0xtest_account_id".to_string(),
            nonce: 1,
            prev_commitment: "prev_commitment".to_string(),
            new_commitment: Some("new_commitment".to_string()),
            delta_payload: serde_json::json!({"test": "payload"}),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::default(),
            metadata: None,
        }
    }

    fn create_test_state_object() -> StateObject {
        StateObject {
            account_id: "0xtest_account_id".to_string(),
            commitment: "old_commitment".to_string(),
            state_json: serde_json::json!({"state": "data"}),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            auth_scheme: String::new(),
        }
    }

    fn create_test_metadata() -> AccountMetadata {
        AccountMetadata {
            account_id: "0xtest_account_id".to_string(),
            auth: crate::metadata::auth::Auth::MidenFalconRpo {
                cosigner_commitments: vec![],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    #[tokio::test]
    async fn test_candidate_submit_delta_error() {
        let mock_storage =
            MockStorageBackend::new().with_submit_delta(Err("Storage unavailable".to_string()));
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new().with_get(Ok(Some(create_test_metadata())));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let storage_backend = state.storage.clone();

        let resolved = ResolvedAccount {
            metadata: create_test_metadata(),
            storage: storage_backend,
        };

        let current_state = create_test_state_object();
        let ctx = CommitContext {
            state: &state,
            resolved: &resolved,
            current_state: &current_state,
            now: "2024-01-01T12:00:00Z".to_string(),
        };

        let mut delta = create_test_delta();
        let result = DeltaCommitStrategy::Candidate
            .commit(
                ctx,
                &mut delta,
                serde_json::json!({"new": "state"}),
                "new_commitment",
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, GuardianError::StorageError(_)));
        assert!(err.to_string().contains("Storage unavailable"));
    }

    #[tokio::test]
    async fn candidate_commit_maps_transactional_commitment_mismatch() {
        let mock_storage = MockStorageBackend::new().with_submit_candidate(Ok(
            CandidateSubmission::CommitmentMismatch {
                expected: "current_commitment".to_string(),
            },
        ));
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new().with_get(Ok(Some(create_test_metadata())));
        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );
        let resolved = ResolvedAccount {
            metadata: create_test_metadata(),
            storage: state.storage.clone(),
        };
        let current_state = create_test_state_object();
        let ctx = CommitContext {
            state: &state,
            resolved: &resolved,
            current_state: &current_state,
            now: "2024-01-01T12:00:00Z".to_string(),
        };
        let mut delta = create_test_delta();

        let result = DeltaCommitStrategy::Candidate
            .commit(
                ctx,
                &mut delta,
                serde_json::json!({"new": "state"}),
                "new_commitment",
            )
            .await;

        assert!(matches!(
            result,
            Err(GuardianError::CommitmentMismatch {
                expected,
                actual,
            }) if expected == "current_commitment" && actual == "prev_commitment"
        ));
    }

    #[tokio::test]
    async fn test_optimistic_submit_state_error() {
        let mock_storage =
            MockStorageBackend::new().with_submit_state(Err("State storage failed".to_string()));
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new().with_get(Ok(Some(create_test_metadata())));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let storage_backend = state.storage.clone();

        let resolved = ResolvedAccount {
            metadata: create_test_metadata(),
            storage: storage_backend,
        };

        let current_state = create_test_state_object();
        let ctx = CommitContext {
            state: &state,
            resolved: &resolved,
            current_state: &current_state,
            now: "2024-01-01T12:00:00Z".to_string(),
        };

        let mut delta = create_test_delta();
        let result = DeltaCommitStrategy::Optimistic
            .commit(
                ctx,
                &mut delta,
                serde_json::json!({"new": "state"}),
                "new_commitment",
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, GuardianError::StorageError(_)));
        assert!(err.to_string().contains("State storage failed"));
    }

    #[tokio::test]
    async fn test_optimistic_submit_delta_error() {
        let mock_storage = MockStorageBackend::new()
            .with_submit_state(Ok(()))
            .with_submit_delta(Err("Delta storage failed".to_string()));
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new().with_get(Ok(Some(create_test_metadata())));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let storage_backend = state.storage.clone();

        let resolved = ResolvedAccount {
            metadata: create_test_metadata(),
            storage: storage_backend,
        };

        let current_state = create_test_state_object();
        let ctx = CommitContext {
            state: &state,
            resolved: &resolved,
            current_state: &current_state,
            now: "2024-01-01T12:00:00Z".to_string(),
        };

        let mut delta = create_test_delta();
        let result = DeltaCommitStrategy::Optimistic
            .commit(
                ctx,
                &mut delta,
                serde_json::json!({"new": "state"}),
                "new_commitment",
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, GuardianError::StorageError(_)));
        assert!(err.to_string().contains("Delta storage failed"));
    }

    #[tokio::test]
    async fn test_optimistic_delete_proposal_error_does_not_fail() {
        // Delete proposal errors should be logged but not fail the commit
        let mock_storage = MockStorageBackend::new()
            .with_submit_state(Ok(()))
            .with_submit_delta(Ok(()))
            .with_pull_delta_proposal(Ok(create_test_delta())) // Proposal exists
            .with_delete_delta_proposal(Err("Delete failed".to_string()));
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new().with_get(Ok(Some(create_test_metadata())));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let storage_backend = state.storage.clone();

        let resolved = ResolvedAccount {
            metadata: create_test_metadata(),
            storage: storage_backend,
        };

        let current_state = create_test_state_object();
        let ctx = CommitContext {
            state: &state,
            resolved: &resolved,
            current_state: &current_state,
            now: "2024-01-01T12:00:00Z".to_string(),
        };

        let mut delta = create_test_delta();
        let result = DeltaCommitStrategy::Optimistic
            .commit(
                ctx,
                &mut delta,
                serde_json::json!({"new": "state"}),
                "new_commitment",
            )
            .await;

        // Should succeed even though delete failed
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_candidate_sets_correct_status() {
        let mock_storage = MockStorageBackend::new().with_submit_delta(Ok(()));
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new().with_get(Ok(Some(create_test_metadata())));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage.clone()),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let storage_backend = state.storage.clone();

        let resolved = ResolvedAccount {
            metadata: create_test_metadata(),
            storage: storage_backend,
        };

        let current_state = create_test_state_object();
        let now = "2024-01-01T12:00:00Z".to_string();
        let ctx = CommitContext {
            state: &state,
            resolved: &resolved,
            current_state: &current_state,
            now: now.clone(),
        };

        let mut delta = create_test_delta();
        let result = DeltaCommitStrategy::Candidate
            .commit(
                ctx,
                &mut delta,
                serde_json::json!({"new": "state"}),
                "new_commitment",
            )
            .await;

        assert!(result.is_ok());
        assert!(delta.status.is_candidate());
        assert_eq!(delta.status.timestamp(), &now);
    }

    #[tokio::test]
    async fn test_optimistic_sets_correct_status() {
        let mock_storage = MockStorageBackend::new()
            .with_submit_state(Ok(()))
            .with_submit_delta(Ok(()));
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new().with_get(Ok(Some(create_test_metadata())));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage.clone()),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        let storage_backend = state.storage.clone();

        let resolved = ResolvedAccount {
            metadata: create_test_metadata(),
            storage: storage_backend,
        };

        let current_state = create_test_state_object();
        let now = "2024-01-01T12:00:00Z".to_string();
        let ctx = CommitContext {
            state: &state,
            resolved: &resolved,
            current_state: &current_state,
            now: now.clone(),
        };

        let mut delta = create_test_delta();
        let result = DeltaCommitStrategy::Optimistic
            .commit(
                ctx,
                &mut delta,
                serde_json::json!({"new": "state"}),
                "new_commitment",
            )
            .await;

        assert!(result.is_ok());
        assert!(delta.status.is_canonical());
        assert_eq!(delta.status.timestamp(), &now);
    }

    #[tokio::test]
    async fn test_from_app_state_with_canonicalization() {
        let mock_storage = MockStorageBackend::new();
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new();

        let mut state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(mock_network),
            Arc::new(mock_metadata),
        );

        // Test without canonicalization (optimistic)
        state.canonicalization = None;
        assert!(matches!(
            DeltaCommitStrategy::from_app_state(&state),
            DeltaCommitStrategy::Optimistic
        ));

        // Test with canonicalization (candidate)
        state.canonicalization = Some(crate::canonicalization::CanonicalizationConfig::default());
        assert!(matches!(
            DeltaCommitStrategy::from_app_state(&state),
            DeltaCommitStrategy::Candidate
        ));
    }
}
