use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::delta_object::DeltaObject;
use crate::state_object::StateObject;
use crate::storage::filesystem::FilesystemService;

pub mod filesystem;

/// Storage backend type with configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum StorageType {
    Filesystem,
}

impl Default for StorageType {
    fn default() -> Self {
        Self::Filesystem
    }
}

impl std::fmt::Display for StorageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageType::Filesystem => write!(f, "Filesystem"),
        }
    }
}

/// Storage backend trait for managing account states and deltas
#[async_trait]
pub trait StorageBackend: Send + Sync {
    async fn submit_state(&self, state: &StateObject) -> Result<(), String>;
    async fn submit_delta(&self, delta: &DeltaObject) -> Result<(), String>;
    async fn pull_state(&self, account_id: &str) -> Result<StateObject, String>;
    async fn pull_delta(&self, account_id: &str, nonce: u64) -> Result<DeltaObject, String>;
    async fn pull_deltas_after(
        &self,
        account_id: &str,
        from_nonce: u64,
    ) -> Result<Vec<DeltaObject>, String>;
    async fn has_pending_candidate(&self, account_id: &str) -> Result<bool, String> {
        let deltas = self.pull_deltas_after(account_id, 0).await?;
        Ok(deltas.iter().any(|delta| delta.status.is_candidate()))
    }
    async fn pull_canonical_deltas_after(
        &self,
        account_id: &str,
        from_nonce: u64,
    ) -> Result<Vec<DeltaObject>, String> {
        let deltas = self.pull_deltas_after(account_id, from_nonce).await?;
        Ok(deltas
            .into_iter()
            .filter(|delta| delta.status.is_canonical())
            .collect())
    }
    async fn submit_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String>;
    async fn pull_delta_proposal(
        &self,
        account_id: &str,
        commitment: &str,
    ) -> Result<DeltaObject, String>;
    async fn pull_all_delta_proposals(&self, account_id: &str) -> Result<Vec<DeltaObject>, String>;
    async fn pull_pending_proposals(&self, account_id: &str) -> Result<Vec<DeltaObject>, String> {
        let mut proposals = self.pull_all_delta_proposals(account_id).await?;
        proposals.retain(|proposal| proposal.status.is_pending());
        proposals.sort_by_key(|proposal| proposal.nonce);
        Ok(proposals)
    }
    async fn update_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String>;
    async fn delete_delta_proposal(&self, account_id: &str, commitment: &str)
    -> Result<(), String>;
}

/// Storage registry that maps storage types to their backend implementations
#[derive(Clone)]
pub struct StorageRegistry {
    backends: Arc<HashMap<StorageType, Arc<dyn StorageBackend>>>,
}

impl StorageRegistry {
    pub fn new(backends: HashMap<StorageType, Arc<dyn StorageBackend>>) -> Self {
        Self {
            backends: Arc::new(backends),
        }
    }

    pub async fn with_filesystem(storage_path: std::path::PathBuf) -> Result<Self, String> {
        let fs_storage = FilesystemService::new(storage_path).await?;

        let mut backends = HashMap::new();
        backends.insert(
            StorageType::Filesystem,
            Arc::new(fs_storage) as Arc<dyn StorageBackend>,
        );

        Ok(Self::new(backends))
    }

    pub fn get(&self, storage_type: &StorageType) -> Result<Arc<dyn StorageBackend>, String> {
        self.backends
            .get(storage_type)
            .cloned()
            .ok_or_else(|| format!("No storage backend registered for type: {storage_type}"))
    }
}
