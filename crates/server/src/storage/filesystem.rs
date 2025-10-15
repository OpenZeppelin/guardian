use crate::storage::{AccountMetadata, AccountState, DeltaObject, MetadataStore, StorageBackend};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;

pub struct FilesystemService {
    app_path: PathBuf,
}

/// Filesystem-based metadata store
/// Stores all account metadata in a single JSON file with in-memory cache
pub struct FilesystemMetadataStore {
    file_path: PathBuf,
    /// In-memory cache of account metadata
    cache: Arc<RwLock<HashMap<String, AccountMetadata>>>,
}

impl FilesystemService {
    /// Create a new FilesystemService
    pub async fn new(app_path: PathBuf) -> Result<Self, String> {
        // Validate that base directories exist or can be created
        fs::create_dir_all(&app_path)
            .await
            .map_err(|e| format!("Failed to create app directory: {e}"))?;

        Ok(Self { app_path })
    }

    /// Atomically write a file
    async fn write(&self, app_path: &Path, content: &str) -> Result<(), String> {
        // Ensure parent directories exist
        if let Some(parent) = app_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent directory: {e}"))?;
        }

        // Write to temp file first to ensure atomic operation:
        // If process crashes during write, original file remains intact.
        // The rename operation below is atomic on Unix/Linux.
        let temp_path = app_path.with_extension("tmp");
        let mut file = fs::File::create(&temp_path)
            .await
            .map_err(|e| format!("Failed to create temp file: {e}"))?;

        file.write_all(content.as_bytes())
            .await
            .map_err(|e| format!("Failed to write to temp file: {e}"))?;

        file.sync_all()
            .await
            .map_err(|e| format!("Failed to sync temp file: {e}"))?;

        drop(file);

        // rename temp file to final location
        fs::rename(&temp_path, app_path)
            .await
            .map_err(|e| format!("Failed to rename temp file: {e}"))?;

        Ok(())
    }

    /// Get the path for an account's state file
    fn get_state_path(&self, account_id: &str) -> PathBuf {
        self.app_path.join(account_id).join("state.json")
    }

    /// Get the path for a delta file
    fn get_delta_path(&self, account_id: &str, nonce: u64) -> PathBuf {
        self.app_path
            .join(account_id)
            .join("deltas")
            .join(format!("{nonce}.json"))
    }
}

#[async_trait]
impl StorageBackend for FilesystemService {
    async fn submit_state(&self, state: &AccountState) -> Result<(), String> {
        let content = serde_json::to_string_pretty(state)
            .map_err(|e| format!("Failed to serialize state: {e}"))?;

        let app_path = self.get_state_path(&state.account_id);

        self.write(&app_path, &content).await
    }

    async fn submit_delta(&self, delta: &DeltaObject) -> Result<(), String> {
        let content = serde_json::to_string_pretty(delta)
            .map_err(|e| format!("Failed to serialize delta: {e}"))?;

        let app_path = self.get_delta_path(&delta.account_id, delta.nonce);

        self.write(&app_path, &content).await
    }

    async fn pull_state(&self, account_id: &str) -> Result<AccountState, String> {
        let app_path = self.get_state_path(account_id);

        let content = fs::read_to_string(&app_path)
            .await
            .map_err(|e| format!("Failed to read state file: {e}"))?;

        let state: AccountState = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to deserialize state: {e}"))?;

        Ok(state)
    }

    async fn pull_delta(&self, account_id: &str, nonce: u64) -> Result<DeltaObject, String> {
        let app_path = self.get_delta_path(account_id, nonce);

        let content = fs::read_to_string(&app_path)
            .await
            .map_err(|e| format!("Failed to read delta file: {e}"))?;

        let delta: DeltaObject = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to deserialize delta: {e}"))?;

        Ok(delta)
    }

    async fn list_deltas(&self, account_id: &str) -> Result<Vec<String>, String> {
        let deltas_dir = self.app_path.join(account_id).join("deltas");

        // If directory doesn't exist, return empty list
        if !deltas_dir.exists() {
            return Ok(Vec::new());
        }

        let mut entries = fs::read_dir(&deltas_dir)
            .await
            .map_err(|e| format!("Failed to read deltas directory: {e}"))?;

        let mut deltas = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| format!("Failed to read directory entry: {e}"))?
        {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".json") {
                    deltas.push(name.to_string());
                }
            }
        }

        // Sort by nonce (extract number from filename)
        deltas.sort_by_key(|name| name.trim_end_matches(".json").parse::<u64>().unwrap_or(0));

        Ok(deltas)
    }

    async fn get_delta_head(&self, account_id: &str) -> Result<Option<u64>, String> {
        let deltas = self.list_deltas(account_id).await?;

        if deltas.is_empty() {
            return Ok(None);
        }

        // Parse nonces from filenames and find the maximum
        let mut max_nonce: Option<u64> = None;
        for filename in &deltas {
            if let Some(nonce_str) = filename.strip_suffix(".json") {
                if let Ok(nonce) = nonce_str.parse::<u64>() {
                    max_nonce = Some(max_nonce.map_or(nonce, |current| current.max(nonce)));
                }
            }
        }

        Ok(max_nonce)
    }
}

impl FilesystemMetadataStore {
    /// Create a new FilesystemMetadataStore
    pub async fn new(base_path: PathBuf) -> Result<Self, String> {
        let metadata_dir = base_path.join(".metadata");
        fs::create_dir_all(&metadata_dir)
            .await
            .map_err(|e| format!("Failed to create metadata directory: {e}"))?;

        let file_path = metadata_dir.join("accounts.json");

        let cache = if file_path.exists() {
            let content = fs::read_to_string(&file_path)
                .await
                .map_err(|e| format!("Failed to read metadata file: {e}"))?;

            let accounts: HashMap<String, AccountMetadata> = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse metadata file: {e}"))?;

            Arc::new(RwLock::new(accounts))
        } else {
            Arc::new(RwLock::new(HashMap::new()))
        };

        Ok(Self { file_path, cache })
    }

    /// Persist metadata cache to disk
    async fn persist(&self, cache: &HashMap<String, AccountMetadata>) -> Result<(), String> {
        // Ensure metadata directory exists
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create metadata directory: {e}"))?;
        }

        let content = serde_json::to_string_pretty(cache)
            .map_err(|e| format!("Failed to serialize metadata: {e}"))?;

        // Atomic write using temp file
        let temp_path = self.file_path.with_extension("tmp");
        let mut file = fs::File::create(&temp_path)
            .await
            .map_err(|e| format!("Failed to create temp file: {e}"))?;

        file.write_all(content.as_bytes())
            .await
            .map_err(|e| format!("Failed to write to temp file: {e}"))?;

        file.sync_all()
            .await
            .map_err(|e| format!("Failed to sync temp file: {e}"))?;

        drop(file);

        fs::rename(&temp_path, &self.file_path)
            .await
            .map_err(|e| format!("Failed to rename temp file: {e}"))?;

        Ok(())
    }
}

#[async_trait]
impl MetadataStore for FilesystemMetadataStore {
    async fn get(&self, account_id: &str) -> Result<Option<AccountMetadata>, String> {
        let cache = self.cache.read().await;
        Ok(cache.get(account_id).cloned())
    }

    async fn set(&self, metadata: AccountMetadata) -> Result<(), String> {
        let account_id = metadata.account_id.clone();

        // Update cache
        {
            let mut cache = self.cache.write().await;
            cache.insert(account_id, metadata);
        }

        // Persist to disk
        let cache = self.cache.read().await;
        self.persist(&cache).await
    }

    async fn list(&self) -> Result<Vec<String>, String> {
        let cache = self.cache.read().await;
        Ok(cache.keys().cloned().collect())
    }
}
