pub mod miden;

use crate::metadata::auth::{Auth, Credentials};
use async_trait::async_trait;

#[async_trait]
pub trait NetworkClient: Send + Sync {
    /// Get state commitment in hex format from JSON
    fn get_state_commitment(
        &self,
        account_id: &str,
        state_json: &serde_json::Value,
    ) -> Result<String, String>;

    /// Verify state commitment matches on-chain state
    async fn verify_state(
        &mut self,
        account_id: &str,
        state_json: &serde_json::Value,
    ) -> Result<(), String>;

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

    /// Get delta proposal ID
    fn delta_proposal_id(
        &self,
        account_id: &str,
        nonce: u64,
        delta_payload: &serde_json::Value,
    ) -> Result<String, String>;

    /// Validate account ID format
    fn validate_account_id(&self, account_id: &str) -> Result<(), String>;

    /// Validate that the credential (public key) is authorized for the account
    /// Checks storage slot 0 (single signer) or slot 1 (mapping of cosigners)
    fn validate_credential(
        &self,
        state_json: &serde_json::Value,
        credential: &Credentials,
        auth: &Auth,
    ) -> Result<(), String>;

    /// Determine if account auth should be updated given the state
    async fn should_update_auth(
        &mut self,
        state_json: &serde_json::Value,
        current_auth: &Auth,
    ) -> Result<Option<Auth>, String>;
}

/// Network type
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NetworkType {
    MidenTestnet,
    MidenDevnet,
    MidenLocal,
}

impl NetworkType {
    pub fn from_env(var_name: &str) -> Self {
        let value = std::env::var(var_name).unwrap_or_else(|_| "MidenDevnet".to_string());
        Self::from_name(&value).unwrap_or(Self::MidenDevnet)
    }

    pub fn from_name(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "midenlocal" | "local" => Some(Self::MidenLocal),
            "midentestnet" | "testnet" => Some(Self::MidenTestnet),
            "midendevnet" | "devnet" => Some(Self::MidenDevnet),
            _ => None,
        }
    }

    pub fn rpc_endpoint(&self) -> &str {
        match self {
            NetworkType::MidenTestnet => "https://rpc.testnet.miden.io",
            NetworkType::MidenDevnet => "https://rpc.devnet.miden.io",
            NetworkType::MidenLocal => "http://localhost:57291",
        }
    }
}

impl Default for NetworkType {
    fn default() -> Self {
        Self::MidenLocal
    }
}

impl std::fmt::Display for NetworkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkType::MidenTestnet => write!(f, "MidenTestnet"),
            NetworkType::MidenDevnet => write!(f, "MidenDevnet"),
            NetworkType::MidenLocal => write!(f, "MidenLocal"),
        }
    }
}
