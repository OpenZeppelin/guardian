use crate::network::NetworkType;
use crate::storage::{MetadataStore, StorageRegistry};
use std::sync::Arc;

/// Application state shared across handlers
#[derive(Clone)]
pub struct AppState {
    pub storage: StorageRegistry,
    pub metadata: Arc<dyn MetadataStore>,
    pub network_type: NetworkType,
}

impl AppState {
    /// Validate account ID format based on the network type
    pub fn validate_account_id(&self, account_id: &str) -> Result<(), String> {
        match self.network_type {
            NetworkType::Miden => {
                use miden_objects::account::AccountId;
                AccountId::from_hex(account_id)
                    .map(|_| ())
                    .map_err(|e| format!("Invalid Miden account ID format: {e}"))
            }
        }
    }
}
