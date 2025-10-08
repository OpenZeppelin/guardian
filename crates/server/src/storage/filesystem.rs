use crate::storage::{AccountState, DeltaObject, StorageBackend};
use async_trait::async_trait;
use std::env;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[derive(Clone, Debug)]
pub struct FilesystemConfig {
    pub app_path: PathBuf,
}

impl FilesystemConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Result<Self, String> {
        let app_path = env::var("PSM_APP_PATH")
            .unwrap_or_else(|_| "/var/psm/app".to_string())
            .into();

        Ok(Self { app_path })
    }
}

pub struct FilesystemService {
    config: FilesystemConfig,
}

impl FilesystemService {
    /// Create a new FilesystemService
    pub async fn new(config: FilesystemConfig) -> Result<Self, String> {
        // Validate that base directories exist or can be created
        fs::create_dir_all(&config.app_path)
            .await
            .map_err(|e| format!("Failed to create app directory: {}", e))?;

        Ok(Self { config })
    }

    /// Atomically write a file
    async fn write(&self, app_path: &Path, content: &str) -> Result<(), String> {
        // Ensure parent directories exist
        if let Some(parent) = app_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent directory: {}", e))?;
        }

        // Write to temp file first to ensure atomic operation:
        // If process crashes during write, original file remains intact.
        // The rename operation below is atomic on Unix/Linux.
        let temp_path = app_path.with_extension("tmp");
        let mut file = fs::File::create(&temp_path)
            .await
            .map_err(|e| format!("Failed to create temp file: {}", e))?;

        file.write_all(content.as_bytes())
            .await
            .map_err(|e| format!("Failed to write to temp file: {}", e))?;

        file.sync_all()
            .await
            .map_err(|e| format!("Failed to sync temp file: {}", e))?;

        drop(file);

        // rename temp file to final location
        fs::rename(&temp_path, app_path)
            .await
            .map_err(|e| format!("Failed to rename temp file: {}", e))?;

        Ok(())
    }

    /// Get the app path for an account's state file
    fn get_state_path(&self, account_id: &str) -> PathBuf {
        self.config
            .app_path
            .join(account_id)
            .join("state.json")
    }

    /// Get the app path for a delta file
    fn get_delta_path(&self, account_id: &str, nonce: u64) -> PathBuf {
        self.config
            .app_path
            .join(account_id)
            .join("deltas")
            .join(format!("{}.json", nonce))
    }
}

#[async_trait]
impl StorageBackend for FilesystemService {
    async fn submit_state(&self, state: &AccountState) -> Result<(), String> {
        let content = serde_json::to_string_pretty(state)
            .map_err(|e| format!("Failed to serialize state: {}", e))?;

        let app_path = self.get_state_path(&state.account_id);

        self.write(&app_path, &content).await
    }

    async fn submit_delta(&self, delta: &DeltaObject) -> Result<(), String> {
        let content = serde_json::to_string_pretty(delta)
            .map_err(|e| format!("Failed to serialize delta: {}", e))?;

        let app_path = self.get_delta_path(&delta.account_id, delta.nonce);

        self.write(&app_path, &content).await
    }

    async fn pull_state(&self, account_id: &str) -> Result<AccountState, String> {
        let app_path = self.get_state_path(account_id);

        let content = fs::read_to_string(&app_path)
            .await
            .map_err(|e| format!("Failed to read state file: {}", e))?;

        let state: AccountState = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to deserialize state: {}", e))?;

        Ok(state)
    }

    async fn pull_delta(&self, account_id: &str, nonce: u64) -> Result<DeltaObject, String> {
        let app_path = self.get_delta_path(account_id, nonce);

        let content = fs::read_to_string(&app_path)
            .await
            .map_err(|e| format!("Failed to read delta file: {}", e))?;

        let delta: DeltaObject = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to deserialize delta: {}", e))?;

        Ok(delta)
    }

    async fn list_deltas(&self, account_id: &str) -> Result<Vec<String>, String> {
        let deltas_dir = self.config.app_path.join(account_id).join("deltas");

        // If directory doesn't exist, return empty list
        if !deltas_dir.exists() {
            return Ok(Vec::new());
        }

        let mut entries = fs::read_dir(&deltas_dir)
            .await
            .map_err(|e| format!("Failed to read deltas directory: {}", e))?;

        let mut delta_files = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| format!("Failed to read directory entry: {}", e))?
        {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".json") {
                    delta_files.push(name.to_string());
                }
            }
        }

        // Sort by nonce (extract number from filename)
        delta_files.sort_by_key(|name| {
            name.trim_end_matches(".json")
                .parse::<u64>()
                .unwrap_or(0)
        });

        Ok(delta_files)
    }
}
