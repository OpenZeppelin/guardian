use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::state_object::StateObject;
pub mod filesystem;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub use postgres::run_migrations;

/// Storage backend type with configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum StorageType {
    Filesystem,
    Postgres,
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
            StorageType::Postgres => write!(f, "Postgres"),
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
    async fn delete_delta(&self, account_id: &str, nonce: u64) -> Result<(), String>;
    async fn update_delta_status(
        &self,
        account_id: &str,
        nonce: u64,
        status: DeltaStatus,
    ) -> Result<(), String>;
}
