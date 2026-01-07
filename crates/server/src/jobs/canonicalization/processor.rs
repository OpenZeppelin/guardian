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
            .list()
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to list accounts: {e}")))?;

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
        let account_metadata = self
            .state
            .metadata
            .get(account_id)
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to get metadata: {e}")))?
            .ok_or_else(|| PsmError::InvalidInput("Account metadata not found".to_string()))?;

        let storage_backend = self
            .state
            .storage
            .get(&account_metadata.storage_type)
            .map_err(PsmError::ConfigurationError)?;

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
        let account_metadata = self
            .state
            .metadata
            .get(&delta.account_id)
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to get metadata: {e}")))?
            .ok_or_else(|| PsmError::AccountNotFound(delta.account_id.clone()))?;

        let storage_backend = self
            .state
            .storage
            .get(&account_metadata.storage_type)
            .map_err(PsmError::ConfigurationError)?;

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

        let storage_backend = self
            .state
            .storage
            .get(&account_metadata.storage_type)
            .map_err(PsmError::ConfigurationError)?;

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
        };

        storage_backend
            .submit_state(&updated_state)
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to update account state: {e}")))?;

        let new_auth = {
            let mut client = self.state.network_client.lock().await;
            client
                .should_update_auth(&new_state_json)
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
        canonical_delta.status = DeltaStatus::canonical(now);

        storage_backend
            .submit_delta(&canonical_delta)
            .await
            .map_err(|e| {
                PsmError::StorageError(format!("Failed to update delta as canonical: {e}"))
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
