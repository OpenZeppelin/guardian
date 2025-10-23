pub mod miden;

use crate::metadata::auth::Auth;
use crate::storage::DeltaObject;
use async_trait::async_trait;

#[async_trait]
pub trait NetworkClient: Send + Sync {
    /// Verify state matches on-chain
    async fn verify_state(
        &mut self,
        account_id: &str,
        state_json: &serde_json::Value,
    ) -> Result<String, String>;

    /// Fetch on-chain state proof
    async fn verify_on_chain_state(&mut self, account_id: &str) -> Result<String, String>;

    /// Verify delta is valid for given state
    fn verify_delta(
        &self,
        prev_proof: &str,
        prev_state_json: &serde_json::Value,
        delta_payload: &serde_json::Value,
    ) -> Result<(), String>;

    /// Apply delta to state
    fn apply_delta(
        &self,
        prev_state_json: &serde_json::Value,
        delta_payload: &serde_json::Value,
    ) -> Result<(serde_json::Value, String), String>;

    /// Merge multiple deltas
    fn merge_deltas(
        &self,
        delta_payloads: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, String>;

    /// Validate account ID format
    fn validate_account_id(&self, account_id: &str) -> Result<(), String>;

    /// Check if delta is canonical per on-chain state
    async fn is_canonical(&mut self, delta: &DeltaObject) -> Result<bool, String>;

    /// Determine if account auth should be updated given the state
    async fn should_update_auth(
        &mut self,
        state_json: &serde_json::Value,
    ) -> Result<Option<Auth>, String>;
}

/// Network type
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NetworkType {
    MidenTestnet,
}

impl NetworkType {
    pub fn rpc_endpoint(&self) -> &str {
        match self {
            NetworkType::MidenTestnet => "https://rpc.testnet.miden.io",
        }
    }
}

impl Default for NetworkType {
    fn default() -> Self {
        Self::MidenTestnet
    }
}

impl std::fmt::Display for NetworkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkType::MidenTestnet => write!(f, "MidenTestnet"),
        }
    }
}
