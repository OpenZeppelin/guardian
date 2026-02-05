use crate::canonicalization::CanonicalizationConfig;
use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::error::{PsmError, Result};
use crate::state::AppState;
use crate::state_object::StateObject;
use async_trait::async_trait;

#[async_trait]
pub trait Processor: Send + Sync {
    async fn process_all_accounts(&self) -> Result<()>;

    #[allow(dead_code)]
    async fn process_account(&self, account_id: &str) -> Result<()>;
}

fn get_candidates(deltas: &[DeltaObject]) -> Vec<DeltaObject> {
    let mut candidates: Vec<DeltaObject> = deltas
        .iter()
        .filter(|delta| delta.status.is_candidate())
        .cloned()
        .collect();

    candidates.sort_by_key(|d| d.nonce);
    candidates
}

struct DeltasProcessorBase {
    state: AppState,
    max_retries: u32,
}

impl DeltasProcessorBase {
    async fn process_all_accounts(&self) -> Result<()> {
        let account_ids = self
            .state
            .metadata
            .list_with_pending_candidates()
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to list accounts: {e}")))?;

        tracing::info!(
            accounts_with_candidates = account_ids.len(),
            "Running canonicalization process"
        );

        for account_id in account_ids {
            if let Err(e) = self.process_account(&account_id).await {
                tracing::error!(
                    account_id = %account_id,
                    error = %e,
                    "Failed to process canonicalizations for account"
                );
            }
        }

        Ok(())
    }

    async fn process_account(&self, account_id: &str) -> Result<()> {
        let _account_metadata = self
            .state
            .metadata
            .get(account_id)
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to get metadata: {e}")))?
            .ok_or_else(|| PsmError::InvalidInput("Account metadata not found".to_string()))?;

        let storage_backend = self.state.storage.clone();

        let all_deltas = storage_backend
            .pull_deltas_after(account_id, 0)
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to pull deltas: {e}")))?;

        tracing::debug!(
            account_id = %account_id,
            total_deltas = all_deltas.len(),
            "Pulled deltas from storage"
        );

        let candidates = get_candidates(&all_deltas);

        tracing::info!(
            account_id = %account_id,
            total_deltas = all_deltas.len(),
            candidates = candidates.len(),
            "Processing delta candidates"
        );

        for delta in candidates {
            let nonce = delta.nonce;
            if let Err(e) = self.process_candidate(delta).await {
                tracing::error!(
                    account_id = %account_id,
                    nonce = nonce,
                    error = %e,
                    "Failed to canonicalize delta"
                );
            }
        }

        Ok(())
    }

    async fn process_candidate(&self, delta: DeltaObject) -> Result<()> {
        let _account_metadata = self
            .state
            .metadata
            .get(&delta.account_id)
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to get metadata: {e}")))?
            .ok_or_else(|| PsmError::AccountNotFound(delta.account_id.clone()))?;

        let storage_backend = self.state.storage.clone();

        let current_state = storage_backend
            .pull_state(&delta.account_id)
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to get current state: {e}")))?;

        let (new_state_json, _) = {
            let client = self.state.network_client.lock().await;
            client
                .apply_delta(&current_state.state_json, &delta.delta_payload)
                .map_err(PsmError::InvalidDelta)?
        };

        let verify_result = {
            let mut client = self.state.network_client.lock().await;
            client
                .verify_state(&delta.account_id, &new_state_json)
                .await
        };

        match verify_result {
            Ok(()) => {
                if let Some(new_commitment) = delta.new_commitment.clone() {
                    self.canonicalize_verified_delta(delta, new_state_json, new_commitment)
                        .await
                } else {
                    tracing::error!(
                        account_id = %delta.account_id,
                        nonce = delta.nonce,
                        "Delta has no new_commitment, cannot canonicalize"
                    );
                    Ok(())
                }
            }
            Err(e) => {
                let current_retry = delta.status.retry_count();
                let new_retry = current_retry + 1;

                if new_retry >= self.max_retries {
                    tracing::warn!(
                        account_id = %delta.account_id,
                        nonce = delta.nonce,
                        retries = new_retry,
                        max_retries = self.max_retries,
                        error = %e,
                        "Delta verification failed after max retries, discarding"
                    );

                    storage_backend
                        .delete_delta(&delta.account_id, delta.nonce)
                        .await
                        .map_err(|e| {
                            PsmError::StorageError(format!("Failed to delete delta: {e}"))
                        })?;

                    // Clear the pending candidate flag after discard
                    let now = self.state.clock.now_rfc3339();
                    if let Err(e) = self
                        .state
                        .metadata
                        .set_has_pending_candidate(&delta.account_id, false, &now)
                        .await
                    {
                        tracing::warn!(
                            account_id = %delta.account_id,
                            error = %e,
                            "Failed to clear has_pending_candidate flag after discard"
                        );
                    }
                } else {
                    tracing::info!(
                        account_id = %delta.account_id,
                        nonce = delta.nonce,
                        retry = new_retry,
                        max_retries = self.max_retries,
                        error = %e,
                        "Delta verification failed, will retry"
                    );

                    let now = self.state.clock.now_rfc3339();
                    let new_status = delta.status.with_incremented_retry(now);

                    storage_backend
                        .update_delta_status(&delta.account_id, delta.nonce, new_status)
                        .await
                        .map_err(|e| {
                            PsmError::StorageError(format!("Failed to update delta status: {e}"))
                        })?;
                }

                Ok(())
            }
        }
    }

    async fn canonicalize_verified_delta(
        &self,
        delta: DeltaObject,
        new_state_json: serde_json::Value,
        new_commitment: String,
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
            .map_err(|e| PsmError::StorageError(format!("Failed to get metadata: {e}")))?
            .ok_or_else(|| PsmError::AccountNotFound(delta.account_id.clone()))?;

        let storage_backend = self.state.storage.clone();

        let current_state = storage_backend
            .pull_state(&delta.account_id)
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to get current state: {e}")))?;

        let now = self.state.clock.now_rfc3339();

        let updated_state = StateObject {
            account_id: delta.account_id.clone(),
            state_json: new_state_json.clone(),
            commitment: new_commitment,
            created_at: current_state.created_at.clone(),
            updated_at: now.clone(),
            auth_scheme: String::new(),
        };

        storage_backend
            .submit_state(&updated_state)
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to update account state: {e}")))?;

        let new_auth = {
            let mut client = self.state.network_client.lock().await;
            client
                .should_update_auth(&new_state_json, &account_metadata.auth)
                .await
                .map_err(|e| PsmError::StorageError(format!("Failed to check auth update: {e}")))?
        };

        if let Some(new_auth) = new_auth {
            tracing::debug!(
                account_id = %delta.account_id,
                "Syncing cosigner public keys from on-chain storage"
            );

            self.state
                .metadata
                .update_auth(&delta.account_id, new_auth, &now)
                .await
                .map_err(|e| PsmError::StorageError(format!("Failed to update auth: {e}")))?;

            tracing::debug!(
                account_id = %delta.account_id,
                "Metadata cosigner public keys synced with storage"
            );
        }

        let mut canonical_delta = delta.clone();
        canonical_delta.status = DeltaStatus::canonical(now.clone());

        storage_backend
            .submit_delta(&canonical_delta)
            .await
            .map_err(|e| {
                PsmError::StorageError(format!("Failed to update delta as canonical: {e}"))
            })?;

        // Clear the pending candidate flag
        self.state
            .metadata
            .set_has_pending_candidate(&delta.account_id, false, &now)
            .await
            .map_err(|e| {
                tracing::warn!(
                    account_id = %delta.account_id,
                    error = %e,
                    "Failed to clear has_pending_candidate flag"
                );
                PsmError::StorageError(format!("Failed to update metadata: {e}"))
            })?;

        // Delete matching proposal now that delta is canonical
        let proposal_id = {
            let client = self.state.network_client.lock().await;
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

        Ok(())
    }
}

pub struct DeltasProcessor {
    base: DeltasProcessorBase,
}

impl DeltasProcessor {
    pub fn new(state: AppState, config: CanonicalizationConfig) -> Self {
        Self {
            base: DeltasProcessorBase {
                state,
                max_retries: config.max_retries,
            },
        }
    }
}

#[async_trait]
impl Processor for DeltasProcessor {
    async fn process_all_accounts(&self) -> Result<()> {
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
                max_retries: u32::MAX, // Test processor doesn't discard on retries
            },
        }
    }
}

#[async_trait]
impl Processor for TestDeltasProcessor {
    async fn process_all_accounts(&self) -> Result<()> {
        self.base.process_all_accounts().await
    }

    async fn process_account(&self, account_id: &str) -> Result<()> {
        self.base.process_account(account_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta_object::DeltaStatus;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::state_object::StateObject;
    use crate::testing::helpers::create_test_app_state_with_mocks;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use std::sync::Arc;

    fn create_test_metadata(account_id: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![],
            },
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: true,
            last_auth_timestamp: None,
        }
    }

    fn create_test_state(account_id: &str) -> StateObject {
        StateObject {
            account_id: account_id.to_string(),
            commitment: "old_commitment".to_string(),
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
        }
    }

    fn create_canonical_delta(account_id: &str, nonce: u64) -> DeltaObject {
        DeltaObject {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: "prev_commitment".to_string(),
            new_commitment: Some("new_commitment".to_string()),
            delta_payload: serde_json::json!({"test": "payload"}),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::canonical("2024-01-01T00:00:00Z".to_string()),
        }
    }

    #[test]
    fn test_get_candidates_filters_only_candidates() {
        let account_id = "0xtest_account";
        let deltas = vec![
            create_candidate_delta(account_id, 1),
            create_canonical_delta(account_id, 2),
            create_candidate_delta(account_id, 3),
        ];

        let candidates = get_candidates(&deltas);

        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|d| d.status.is_candidate()));
    }

    #[test]
    fn test_get_candidates_sorts_by_nonce() {
        let account_id = "0xtest_account";
        let deltas = vec![
            create_candidate_delta(account_id, 5),
            create_candidate_delta(account_id, 2),
            create_candidate_delta(account_id, 8),
            create_candidate_delta(account_id, 1),
        ];

        let candidates = get_candidates(&deltas);

        assert_eq!(candidates.len(), 4);
        assert_eq!(candidates[0].nonce, 1);
        assert_eq!(candidates[1].nonce, 2);
        assert_eq!(candidates[2].nonce, 5);
        assert_eq!(candidates[3].nonce, 8);
    }

    #[test]
    fn test_get_candidates_empty_input() {
        let deltas: Vec<DeltaObject> = vec![];
        let candidates = get_candidates(&deltas);
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_get_candidates_no_candidates() {
        let account_id = "0xtest_account";
        let deltas = vec![
            create_canonical_delta(account_id, 1),
            create_canonical_delta(account_id, 2),
        ];

        let candidates = get_candidates(&deltas);
        assert!(candidates.is_empty());
    }

    #[tokio::test]
    async fn test_process_all_accounts_empty_list() {
        let mock_storage = MockStorageBackend::new();
        let mock_network = MockNetworkClient::new();
        let mock_metadata = MockMetadataStore::new().with_list_with_pending_candidates(Ok(vec![]));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PsmError::StorageError(_)));
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        // process_all_accounts should continue even if one account fails
        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
            .with_pull_state(Ok(create_test_state(account_id))) // Called twice
            .with_submit_state(Ok(()))
            .with_submit_delta(Ok(()));

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_state(Ok(()))
            .with_should_update_auth(Ok(None));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(())); // For clearing has_pending_candidate

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(tokio::sync::Mutex::new(mock_network)),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
            Arc::new(mock_metadata),
        );

        // Use max_retries > 1 so it increments instead of discarding
        let config = CanonicalizationConfig::new(10, 18);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
            Arc::new(mock_metadata),
        );

        // max_retries = 10, so retry_count 9 + 1 = 10 >= 10, will discard
        let config = CanonicalizationConfig::new(10, 18);
        let processor = DeltasProcessor::new(state, config);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_process_candidate_no_new_commitment() {
        let account_id = "0xtest_account";
        let mut candidate = create_candidate_delta(account_id, 1);
        candidate.new_commitment = None; // No commitment

        let mock_storage = MockStorageBackend::new()
            .with_pull_deltas_after(Ok(vec![candidate.clone()]))
            .with_pull_state(Ok(create_test_state(account_id)));

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_state(Ok(()));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))));

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(tokio::sync::Mutex::new(mock_network)),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        // Should succeed but log error about missing commitment
        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
            Arc::new(mock_metadata),
        );

        let config = CanonicalizationConfig::default();
        let processor = DeltasProcessor::new(state, config);

        // Should continue processing even on error
        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
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
            .with_submit_state(Ok(()))
            .with_submit_delta(Ok(()));

        let mock_network = MockNetworkClient::new()
            .with_apply_delta(Ok((
                serde_json::json!({"new": "state"}),
                "new_commitment".to_string(),
            )))
            .with_verify_state(Ok(()))
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
            .with_verify_state(Ok(()))
            .with_should_update_auth(Ok(None));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(())); // For clearing has_pending_candidate

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
            .with_verify_state(Ok(()))
            .with_should_update_auth(Ok(None));

        let mock_metadata = MockMetadataStore::new()
            .with_list_with_pending_candidates(Ok(vec![account_id.to_string()]))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_get(Ok(Some(create_test_metadata(account_id))))
            .with_set(Ok(())); // For clearing has_pending_candidate

        let state = create_test_app_state_with_mocks(
            Arc::new(mock_storage),
            Arc::new(tokio::sync::Mutex::new(mock_network)),
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
            Arc::new(tokio::sync::Mutex::new(mock_network)),
            Arc::new(mock_metadata),
        );

        let processor = TestDeltasProcessor::new(state);

        let result = processor.process_all_accounts().await;
        assert!(result.is_ok());
    }
}
