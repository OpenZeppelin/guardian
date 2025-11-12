use crate::delta_object::DeltaObject;
use crate::state_object::StateObject;
use crate::storage::StorageBackend;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

pub struct FilesystemService {
    app_path: PathBuf,
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

    /// Get the path for a delta proposal file
    fn get_delta_proposal_path(&self, account_id: &str, commitment: &str) -> PathBuf {
        // Remove 0x prefix if present
        let clean_commitment = commitment.strip_prefix("0x").unwrap_or(commitment);
        self.app_path
            .join(account_id)
            .join("proposals")
            .join(format!("{clean_commitment}.json"))
    }

    async fn list_delta_filenames(&self, account_id: &str) -> Result<Vec<String>, String> {
        let deltas_dir = self.app_path.join(account_id).join("deltas");

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
            if let Some(name) = entry.file_name().to_str()
                && name.ends_with(".json")
            {
                deltas.push(name.to_string());
            }
        }

        deltas.sort_by_key(|name| name.trim_end_matches(".json").parse::<u64>().unwrap_or(0));

        Ok(deltas)
    }

    async fn list_proposal_filenames(&self, account_id: &str) -> Result<Vec<String>, String> {
        let proposals_dir = self.app_path.join(account_id).join("proposals");

        if !proposals_dir.exists() {
            return Ok(Vec::new());
        }

        let mut entries = fs::read_dir(&proposals_dir)
            .await
            .map_err(|e| format!("Failed to read proposals directory: {e}"))?;

        let mut proposals = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| format!("Failed to read directory entry: {e}"))?
        {
            if let Some(name) = entry.file_name().to_str()
                && name.ends_with(".json")
            {
                proposals.push(name.to_string());
            }
        }

        // Sort alphabetically by filename (works for hex commitments)
        proposals.sort();

        Ok(proposals)
    }
}

#[async_trait]
impl StorageBackend for FilesystemService {
    async fn submit_state(&self, state: &StateObject) -> Result<(), String> {
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

    async fn pull_state(&self, account_id: &str) -> Result<StateObject, String> {
        let app_path = self.get_state_path(account_id);

        let content = fs::read_to_string(&app_path)
            .await
            .map_err(|e| format!("Failed to read state file: {e}"))?;

        let state: StateObject = serde_json::from_str(&content)
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

    async fn pull_deltas_after(
        &self,
        account_id: &str,
        from_nonce: u64,
    ) -> Result<Vec<DeltaObject>, String> {
        let deltas_filenames = self.list_delta_filenames(account_id).await?;

        let mut deltas = Vec::new();
        for filename in deltas_filenames {
            if let Some(nonce_str) = filename.strip_suffix(".json")
                && let Ok(nonce) = nonce_str.parse::<u64>()
                && nonce >= from_nonce
            {
                let delta = self.pull_delta(account_id, nonce).await?;
                deltas.push(delta);
            }
        }

        // Sort by nonce to ensure correct merge order
        deltas.sort_by_key(|d| d.nonce);

        Ok(deltas)
    }

    // Delta proposal methods - stored separately from executed deltas
    async fn submit_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String> {
        let path = self.get_delta_proposal_path(&proposal.account_id, commitment);

        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create proposals directory: {e}"))?;
        }

        // Write to temp file first
        let temp_path = path.with_extension("tmp");
        let json = serde_json::to_string_pretty(&proposal)
            .map_err(|e| format!("Failed to serialize proposal: {e}"))?;

        fs::write(&temp_path, json)
            .await
            .map_err(|e| format!("Failed to write proposal file: {e}"))?;

        // Atomic rename
        fs::rename(&temp_path, &path)
            .await
            .map_err(|e| format!("Failed to finalize proposal file: {e}"))?;

        Ok(())
    }

    async fn pull_delta_proposal(
        &self,
        account_id: &str,
        commitment: &str,
    ) -> Result<DeltaObject, String> {
        let path = self.get_delta_proposal_path(account_id, commitment);

        let json = fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Failed to read proposal file: {e}"))?;

        let proposal: DeltaObject =
            serde_json::from_str(&json).map_err(|e| format!("Failed to parse proposal: {e}"))?;

        Ok(proposal)
    }

    async fn pull_all_delta_proposals(&self, account_id: &str) -> Result<Vec<DeltaObject>, String> {
        let proposal_filenames = self.list_proposal_filenames(account_id).await?;

        let mut proposals = Vec::new();
        for filename in proposal_filenames {
            if let Some(commitment) = filename.strip_suffix(".json") {
                match self.pull_delta_proposal(account_id, commitment).await {
                    Ok(proposal) => proposals.push(proposal),
                    Err(e) => {
                        // Log error but continue loading other proposals
                        tracing::warn!("Failed to load proposal {}: {}", filename, e);
                    }
                }
            }
        }

        // Proposals will be sorted and filtered by the service layer
        Ok(proposals)
    }

    async fn update_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String> {
        // For filesystem, update is the same as submit
        self.submit_delta_proposal(commitment, proposal).await
    }

    async fn delete_delta_proposal(
        &self,
        account_id: &str,
        commitment: &str,
    ) -> Result<(), String> {
        let path = self.get_delta_proposal_path(account_id, commitment);

        // Check if the file exists
        if !path.exists() {
            return Ok(()); // Already deleted or doesn't exist
        }

        // Delete the proposal file
        fs::remove_file(&path)
            .await
            .map_err(|e| format!("Failed to delete proposal file: {e}"))?;

        Ok(())
    }
}
