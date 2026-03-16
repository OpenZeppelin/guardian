use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::error::PsmError;
use crate::services::ResolvedAccount;
use crate::state::AppState;
use crate::state_object::StateObject;
use tracing::{error, info, warn};

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
    ) -> Result<(), PsmError> {
        match self {
            DeltaCommitStrategy::Candidate => {
                delta.status = DeltaStatus::candidate(ctx.now.clone());
                ctx.resolved
                    .storage
                    .submit_delta(delta)
                    .await
                    .map_err(|e| {
                        error!(
                            account_id = %delta.account_id,
                            nonce = delta.nonce,
                            error = %e,
                            "Failed to submit candidate delta"
                        );
                        PsmError::StorageError(format!("Failed to submit delta: {e}"))
                    })?;

                // Set flag indicating account has a pending candidate
                ctx.state
                    .metadata
                    .set_has_pending_candidate(&delta.account_id, true, &ctx.now)
                    .await
                    .map_err(|e| {
                        warn!(
                            account_id = %delta.account_id,
                            error = %e,
                            "Failed to set has_pending_candidate flag"
                        );
                        PsmError::StorageError(format!("Failed to update metadata: {e}"))
                    })
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
                        PsmError::StorageError(format!("Failed to update state: {e}"))
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
                        PsmError::StorageError(format!("Failed to submit delta: {e}"))
                    })?;

                // Delete matching proposal now that delta is canonical
                let proposal_id = {
                    let client = ctx.state.network_client.lock().await;
                    client
                        .delta_proposal_id(&delta.account_id, delta.nonce, &delta.delta_payload)
                        .ok()
                };

                if let Some(ref id) = proposal_id
                    && let Ok(_existing_proposal) = ctx
                        .resolved
                        .storage
                        .pull_delta_proposal(&delta.account_id, id)
                        .await
                {
                    info!(
                        account_id = %delta.account_id,
                        proposal_id = %id,
                        "Deleting matching proposal as delta is now canonical"
                    );
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
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
        assert!(matches!(err, PsmError::StorageError(_)));
        assert!(err.to_string().contains("Storage unavailable"));
    }

    #[tokio::test]
    async fn test_optimistic_submit_state_error() {
        let mock_storage =
            MockStorageBackend::new().with_submit_state(Err("State storage failed".to_string()));
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new().with_get(Ok(Some(create_test_metadata())));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
        assert!(matches!(err, PsmError::StorageError(_)));
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
        assert!(matches!(err, PsmError::StorageError(_)));
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
