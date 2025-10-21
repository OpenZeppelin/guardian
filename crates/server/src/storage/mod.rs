use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::auth::Auth;
use crate::storage::filesystem::FilesystemService;

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

/// Delta status state machine
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeltaStatus {
    Candidate { timestamp: String },
    Canonical { timestamp: String },
    Discarded { timestamp: String },
}

impl DeltaStatus {
    pub fn candidate(timestamp: String) -> Self {
        Self::Candidate { timestamp }
    }

    pub fn canonical(timestamp: String) -> Self {
        Self::Canonical { timestamp }
    }

    pub fn discarded(timestamp: String) -> Self {
        Self::Discarded { timestamp }
    }

    pub fn is_candidate(&self) -> bool {
        matches!(self, Self::Candidate { .. })
    }

    pub fn is_canonical(&self) -> bool {
        matches!(self, Self::Canonical { .. })
    }

    pub fn is_discarded(&self) -> bool {
        matches!(self, Self::Discarded { .. })
    }

    pub fn timestamp(&self) -> &str {
        match self {
            Self::Candidate { timestamp } => timestamp,
            Self::Canonical { timestamp } => timestamp,
            Self::Discarded { timestamp } => timestamp,
        }
    }
}

impl Default for DeltaStatus {
    fn default() -> Self {
        Self::Candidate {
            timestamp: String::new(),
        }
    }
}

/// Delta object
#[derive(Serialize, Clone, Debug, Default)]
pub struct DeltaObject {
    pub account_id: String,
    pub nonce: u64,
    pub prev_commitment: String,
    #[serde(default)]
    pub new_commitment: String,
    pub delta_payload: serde_json::Value,
    pub ack_sig: Option<String>,
    pub status: DeltaStatus,
}

impl<'de> Deserialize<'de> for DeltaObject {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct DeltaObjectHelper {
            account_id: String,
            nonce: u64,
            prev_commitment: String,
            #[serde(default)]
            new_commitment: String,
            delta_payload: serde_json::Value,
            ack_sig: Option<String>,
            #[serde(default)]
            status: Option<DeltaStatus>,
            #[serde(default)]
            candidate_at: Option<String>,
            #[serde(default)]
            canonical_at: Option<String>,
            #[serde(default)]
            discarded_at: Option<String>,
        }

        let helper = DeltaObjectHelper::deserialize(deserializer)?;

        let status = if let Some(status) = helper.status {
            status
        } else if let Some(discarded_at) = helper.discarded_at {
            DeltaStatus::discarded(discarded_at)
        } else if let Some(canonical_at) = helper.canonical_at {
            DeltaStatus::canonical(canonical_at)
        } else if let Some(candidate_at) = helper.candidate_at {
            DeltaStatus::candidate(candidate_at)
        } else {
            DeltaStatus::default()
        };

        Ok(DeltaObject {
            account_id: helper.account_id,
            nonce: helper.nonce,
            prev_commitment: helper.prev_commitment,
            new_commitment: helper.new_commitment,
            delta_payload: helper.delta_payload,
            ack_sig: helper.ack_sig,
            status,
        })
    }
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

    /// Pull all deltas after a given nonce
    async fn pull_deltas_after(
        &self,
        account_id: &str,
        from_nonce: u64,
    ) -> Result<Vec<DeltaObject>, String>;

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
        let fs_storage = FilesystemService::new(storage_path).await?;

        let mut backends = HashMap::new();
        backends.insert(
            StorageType::Filesystem,
            Arc::new(fs_storage) as Arc<dyn StorageBackend>,
        );

        Ok(Self::new(backends))
    }

    /// Get a storage backend for a specific storage type
    pub fn get(&self, storage_type: &StorageType) -> Result<Arc<dyn StorageBackend>, String> {
        self.backends
            .get(storage_type)
            .cloned()
            .ok_or_else(|| format!("No storage backend registered for type: {storage_type}"))
    }
}
