use crate::auth;
use crate::canonicalization::CanonicalizationConfig;
use crate::error::{PsmError, Result};
use crate::state::AppState;
use crate::storage::{AccountState, DeltaObject, DeltaStatus};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[async_trait]
pub trait Processor: Send + Sync {
    async fn process_all_accounts(&self) -> Result<()>;
    async fn process_account(&self, account_id: &str) -> Result<()>;
}

pub struct DeltasProcessor {
    state: AppState,
    config: CanonicalizationConfig,
}

impl DeltasProcessor {
    pub fn new(state: AppState, config: CanonicalizationConfig) -> Self {
        Self { state, config }
    }

    fn filter_ready_candidates(&self, deltas: &[DeltaObject]) -> Vec<DeltaObject> {
        let now = Utc::now();
        let mut candidates: Vec<DeltaObject> = deltas
            .iter()
            .filter(|delta| self.is_ready_candidate(delta, &now))
            .cloned()
            .collect();

        candidates.sort_by_key(|d| d.nonce);
        candidates
    }

    fn is_ready_candidate(&self, delta: &DeltaObject, now: &DateTime<Utc>) -> bool {
        if !delta.status.is_candidate() {
            return false;
        }

        let candidate_at_str = delta.status.timestamp();
        if let Ok(candidate_at) = DateTime::parse_from_rfc3339(candidate_at_str) {
            let elapsed = now.signed_duration_since(candidate_at);
            return elapsed.num_seconds() >= self.config.delay_seconds as i64;
        }

        false
    }
}

#[async_trait]
impl Processor for DeltasProcessor {
    async fn process_all_accounts(&self) -> Result<()> {
        let account_ids = self
            .state
            .metadata
            .list()
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to list accounts: {e}")))?;

        for account_id in account_ids {
            if let Err(e) = self.process_account(&account_id).await {
                eprintln!("Failed to process canonicalizations for account {account_id}: {e}");
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

        let candidates = self.filter_ready_candidates(&all_deltas);

        for delta in candidates {
            let nonce = delta.nonce;
            if let Err(e) = self.process_candidate(delta).await {
                eprintln!("Failed to canonicalize delta {nonce} for account {account_id}: {e}");
            }
        }

        Ok(())
    }
}

impl DeltasProcessor {
    async fn process_candidate(&self, delta: DeltaObject) -> Result<()> {
        let is_canonical = {
            let mut client = self.state.network_client.lock().await;
            client
                .is_canonical(&delta)
                .await
                .map_err(PsmError::NetworkError)?
        };

        if is_canonical {
            self.canonicalize_verified_delta(delta).await
        } else {
            self.discard_mismatched_delta(delta).await
        }
    }

    async fn canonicalize_verified_delta(&self, delta: DeltaObject) -> Result<()> {
        println!(
            "✓ Canonicalizing delta {} for account {} (commitment matches on-chain)",
            delta.nonce, delta.account_id
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

        let (new_state_json, new_commitment) = {
            let client = self.state.network_client.lock().await;
            client
                .apply_delta(&current_state.state_json, &delta.delta_payload)
                .map_err(PsmError::InvalidDelta)?
        };

        let now = chrono::Utc::now().to_rfc3339();

        let updated_state = AccountState {
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
            println!("  Syncing cosigner public keys from on-chain storage");

            auth::update_credentials(&*self.state.metadata, &delta.account_id, new_auth, &now)
                .await?;

            println!("  ✓ Metadata cosigner public keys synced with storage");
        }

        let mut canonical_delta = delta.clone();
        canonical_delta.status = DeltaStatus::canonical(now);

        storage_backend
            .submit_delta(&canonical_delta)
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to update delta as canonical: {e}")))?;

        Ok(())
    }

    async fn discard_mismatched_delta(&self, delta: DeltaObject) -> Result<()> {
        println!(
            "✗ Discarding delta {} for account {} (commitment mismatch with on-chain state)",
            delta.nonce, delta.account_id
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

        let now = chrono::Utc::now().to_rfc3339();

        let mut discarded_delta = delta.clone();
        discarded_delta.status = DeltaStatus::discarded(now);

        storage_backend
            .submit_delta(&discarded_delta)
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to update delta as discarded: {e}")))?;

        Ok(())
    }
}

pub struct TestDeltasProcessor {
    state: AppState,
}

impl TestDeltasProcessor {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    fn filter_pending_candidates(&self, deltas: &[DeltaObject]) -> Vec<DeltaObject> {
        let mut candidates: Vec<DeltaObject> = deltas
            .iter()
            .filter(|delta| delta.status.is_candidate())
            .cloned()
            .collect();

        candidates.sort_by_key(|d| d.nonce);
        candidates
    }
}

#[async_trait]
impl Processor for TestDeltasProcessor {
    async fn process_all_accounts(&self) -> Result<()> {
        let account_ids = self
            .state
            .metadata
            .list()
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to list accounts: {e}")))?;

        for account_id in account_ids {
            if let Err(e) = self.process_account(&account_id).await {
                eprintln!("Failed to process canonicalizations for account {account_id}: {e}");
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

        let candidates = self.filter_pending_candidates(&all_deltas);

        for delta in candidates {
            let nonce = delta.nonce;
            if let Err(e) = self.process_candidate(delta).await {
                eprintln!("Failed to canonicalize delta {nonce} for account {account_id}: {e}");
            }
        }

        Ok(())
    }
}

impl TestDeltasProcessor {
    async fn process_candidate(&self, delta: DeltaObject) -> Result<()> {
        let is_canonical = {
            let mut client = self.state.network_client.lock().await;
            client
                .is_canonical(&delta)
                .await
                .map_err(PsmError::NetworkError)?
        };

        if is_canonical {
            self.canonicalize_verified_delta(delta).await
        } else {
            self.discard_mismatched_delta(delta).await
        }
    }

    async fn canonicalize_verified_delta(&self, delta: DeltaObject) -> Result<()> {
        println!(
            "✓ Canonicalizing delta {} for account {} (commitment matches on-chain)",
            delta.nonce, delta.account_id
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

        let (new_state_json, new_commitment) = {
            let client = self.state.network_client.lock().await;
            client
                .apply_delta(&current_state.state_json, &delta.delta_payload)
                .map_err(PsmError::InvalidDelta)?
        };

        let now = chrono::Utc::now().to_rfc3339();

        let updated_state = AccountState {
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
            println!("  Syncing cosigner public keys from on-chain storage");

            auth::update_credentials(&*self.state.metadata, &delta.account_id, new_auth, &now)
                .await?;

            println!("  ✓ Metadata cosigner public keys synced with storage");
        }

        let mut canonical_delta = delta.clone();
        canonical_delta.status = DeltaStatus::canonical(now);

        storage_backend
            .submit_delta(&canonical_delta)
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to update delta as canonical: {e}")))?;

        Ok(())
    }

    async fn discard_mismatched_delta(&self, delta: DeltaObject) -> Result<()> {
        println!(
            "✗ Discarding delta {} for account {} (commitment mismatch with on-chain state)",
            delta.nonce, delta.account_id
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

        let now = chrono::Utc::now().to_rfc3339();

        let mut discarded_delta = delta.clone();
        discarded_delta.status = DeltaStatus::discarded(now);

        storage_backend
            .submit_delta(&discarded_delta)
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to update delta as discarded: {e}")))?;

        Ok(())
    }
}
