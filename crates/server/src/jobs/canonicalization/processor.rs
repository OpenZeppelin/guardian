use crate::canonicalization::CanonicalizationConfig;
use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::error::{PsmError, Result};
use crate::state::AppState;
use crate::state_object::StateObject;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[async_trait]
pub trait Processor: Send + Sync {
    async fn process_all_accounts(&self) -> Result<()>;

    #[allow(dead_code)]
    async fn process_account(&self, account_id: &str) -> Result<()>;
}

trait CandidateFilter: Send + Sync {
    fn filter(&self, deltas: &[DeltaObject]) -> Vec<DeltaObject>;
}

struct TimeBasedFilter {
    config: CanonicalizationConfig,
    now: DateTime<Utc>,
}

impl CandidateFilter for TimeBasedFilter {
    fn filter(&self, deltas: &[DeltaObject]) -> Vec<DeltaObject> {
        let mut candidates: Vec<DeltaObject> = deltas
            .iter()
            .filter(|delta| self.is_ready_candidate(delta))
            .cloned()
            .collect();

        candidates.sort_by_key(|d| d.nonce);
        candidates
    }
}

impl TimeBasedFilter {
    fn is_ready_candidate(&self, delta: &DeltaObject) -> bool {
        if !delta.status.is_candidate() {
            return false;
        }

        let candidate_at_str = delta.status.timestamp();
        if let Ok(candidate_at) = DateTime::parse_from_rfc3339(candidate_at_str) {
            let elapsed = self.now.signed_duration_since(candidate_at);
            return elapsed.num_seconds() >= self.config.delay_seconds as i64;
        }

        false
    }
}

struct AllCandidatesFilter;

impl CandidateFilter for AllCandidatesFilter {
    fn filter(&self, deltas: &[DeltaObject]) -> Vec<DeltaObject> {
        let mut candidates: Vec<DeltaObject> = deltas
            .iter()
            .filter(|delta| delta.status.is_candidate())
            .cloned()
            .collect();

        candidates.sort_by_key(|d| d.nonce);
        candidates
    }
}

struct DeltasProcessorBase {
    state: AppState,
}

impl DeltasProcessorBase {
    async fn process_all_with_filter(&self, filter: &dyn CandidateFilter) -> Result<()> {
        let account_ids = self
            .state
            .metadata
            .list()
            .await
            .map_err(|e| PsmError::StorageError(format!("Failed to list accounts: {e}")))?;

        for account_id in account_ids {
            if let Err(e) = self.process_account_with_filter(&account_id, filter).await {
                tracing::error!(
                    account_id = %account_id,
                    error = %e,
                    "Failed to process canonicalizations for account"
                );
            }
        }

        Ok(())
    }

    async fn process_account_with_filter(
        &self,
        account_id: &str,
        filter: &dyn CandidateFilter,
    ) -> Result<()> {
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

        let candidates = filter.filter(&all_deltas);

        tracing::info!(
            account_id = %account_id,
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
                let new_commitment = delta.new_commitment.clone();
                self.canonicalize_verified_delta(delta, new_state_json, new_commitment)
                    .await
            }
            _ => self.discard_mismatched_delta(delta).await,
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

        Ok(())
    }

    async fn discard_mismatched_delta(&self, delta: DeltaObject) -> Result<()> {
        tracing::warn!(
            account_id = %delta.account_id,
            nonce = delta.nonce,
            "Discarding delta (commitment mismatch with on-chain state)"
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

        let now = self.state.clock.now_rfc3339();

        let mut discarded_delta = delta.clone();
        discarded_delta.status = DeltaStatus::discarded(now);

        storage_backend
            .submit_delta(&discarded_delta)
            .await
            .map_err(|e| {
                PsmError::StorageError(format!("Failed to update delta as discarded: {e}"))
            })?;

        Ok(())
    }
}

pub struct DeltasProcessor {
    base: DeltasProcessorBase,
    config: CanonicalizationConfig,
}

impl DeltasProcessor {
    pub fn new(state: AppState, config: CanonicalizationConfig) -> Self {
        Self {
            base: DeltasProcessorBase { state },
            config,
        }
    }
}

#[async_trait]
impl Processor for DeltasProcessor {
    async fn process_all_accounts(&self) -> Result<()> {
        let filter = TimeBasedFilter {
            config: self.config.clone(),
            now: self.base.state.clock.now(),
        };
        self.base.process_all_with_filter(&filter).await
    }

    async fn process_account(&self, account_id: &str) -> Result<()> {
        let filter = TimeBasedFilter {
            config: self.config.clone(),
            now: self.base.state.clock.now(),
        };
        self.base
            .process_account_with_filter(account_id, &filter)
            .await
    }
}

pub struct TestDeltasProcessor {
    base: DeltasProcessorBase,
}

impl TestDeltasProcessor {
    pub fn new(state: AppState) -> Self {
        Self {
            base: DeltasProcessorBase { state },
        }
    }
}

#[async_trait]
impl Processor for TestDeltasProcessor {
    async fn process_all_accounts(&self) -> Result<()> {
        let filter = AllCandidatesFilter;
        self.base.process_all_with_filter(&filter).await
    }

    async fn process_account(&self, account_id: &str) -> Result<()> {
        let filter = AllCandidatesFilter;
        self.base
            .process_account_with_filter(account_id, &filter)
            .await
    }
}
