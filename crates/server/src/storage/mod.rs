use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod filesystem;

/// Account state object
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AccountState {
    pub account_id: String,
    pub state_json: serde_json::Value,
    pub commitment: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Delta object
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct DeltaObject {
    pub account_id: String,
    pub nonce: u64,
    pub prev_commitment: String,
    pub delta_hash: String,
    pub delta_payload: serde_json::Value,
    pub ack_sig: String,
    pub candidate_at: String,
    pub canonical_at: Option<String>,
    pub discarded_at: Option<String>,
}

/// Storage backend trait for managing account states and deltas
#[async_trait]
pub trait StorageBackend: Send + Sync {
    /// Submit an account state
    async fn submit_state(&self, state: &AccountState) -> Result<(), String>;

    /// Submit a delta
    async fn submit_delta(&self, delta: &DeltaObject) -> Result<(), String>;

    /// Pull account state
    async fn pull_state(&self, account_id: &str) -> Result<AccountState, String>;

    /// Pull a specific delta
    async fn pull_delta(&self, account_id: &str, nonce: u64) -> Result<DeltaObject, String>;

    /// List all deltas for an account
    async fn list_deltas(&self, account_id: &str) -> Result<Vec<String>, String>;
}
