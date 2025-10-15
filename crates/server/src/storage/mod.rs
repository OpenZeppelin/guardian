use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::auth::Auth;

pub mod filesystem;

/// Storage backend type with configuration
/// Each variant contains storage-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum StorageType {
    /// Filesystem-based storage (local disk)
    Filesystem,
    // Future options with configs:
    // S3 { bucket: String, region: String },
    // PostgreSQL { connection_string: String },
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

/// Metadata for a single account
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AccountMetadata {
    pub account_id: String,
    pub auth: Auth,
    pub storage_type: StorageType,
    pub created_at: String,
    pub updated_at: String,
}

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

    /// Get the latest nonce for an account (returns None if no deltas exist)
    async fn get_delta_head(&self, account_id: &str) -> Result<Option<u64>, String>;
}

/// Metadata store trait for managing account metadata
#[async_trait]
pub trait MetadataStore: Send + Sync {
    /// Get metadata for a specific account
    async fn get(&self, account_id: &str) -> Result<Option<AccountMetadata>, String>;

    /// Store or update metadata for an account
    async fn set(&self, metadata: AccountMetadata) -> Result<(), String>;

    /// List all account IDs
    async fn list(&self) -> Result<Vec<String>, String>;
}

/// Storage registry that maps storage types to their backend implementations
#[derive(Clone)]
pub struct StorageRegistry {
    backends: Arc<HashMap<StorageType, Arc<dyn StorageBackend>>>,
}

impl StorageRegistry {
    /// Create a new storage registry from a map of storage types to backends
    pub fn new(backends: HashMap<StorageType, Arc<dyn StorageBackend>>) -> Self {
        Self {
            backends: Arc::new(backends),
        }
    }

    /// Create a storage registry with only filesystem backend (using default path)
    ///
    /// Uses `/var/psm/storage` as the default storage path.
    /// For custom paths or multiple backends, use `new()` instead.
    pub async fn with_filesystem(storage_path: std::path::PathBuf) -> Result<Self, String> {
        use crate::storage::filesystem::FilesystemService;

        let fs_storage = FilesystemService::new(storage_path).await?;

        let mut backends = HashMap::new();
        backends.insert(StorageType::Filesystem, Arc::new(fs_storage) as Arc<dyn StorageBackend>);

        Ok(Self::new(backends))
    }

    /// Get a storage backend for a specific storage type
    pub fn get(&self, storage_type: &StorageType) -> Result<Arc<dyn StorageBackend>, String> {
        self.backends
            .get(storage_type)
            .cloned()
            .ok_or_else(|| format!("No storage backend registered for type: {}", storage_type))
    }
}
