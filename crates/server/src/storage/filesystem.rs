use crate::delta_object::{DeltaObject, DeltaStatus};
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

    async fn has_pending_candidate(&self, account_id: &str) -> Result<bool, String> {
        let deltas_filenames = self.list_delta_filenames(account_id).await?;
        for filename in deltas_filenames {
            if let Some(nonce_str) = filename.strip_suffix(".json")
                && let Ok(nonce) = nonce_str.parse::<u64>()
                && self
                    .pull_delta(account_id, nonce)
                    .await?
                    .status
                    .is_candidate()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn pull_canonical_deltas_after(
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
                if delta.status.is_canonical() {
                    deltas.push(delta);
                }
            }
        }

        deltas.sort_by_key(|delta| delta.nonce);
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

    async fn pull_pending_proposals(&self, account_id: &str) -> Result<Vec<DeltaObject>, String> {
        let proposal_filenames = self.list_proposal_filenames(account_id).await?;
        let mut proposals = Vec::new();

        for filename in proposal_filenames {
            if let Some(commitment) = filename.strip_suffix(".json") {
                match self.pull_delta_proposal(account_id, commitment).await {
                    Ok(proposal) if proposal.status.is_pending() => proposals.push(proposal),
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("Failed to load proposal {}: {}", filename, e);
                    }
                }
            }
        }

        proposals.sort_by_key(|proposal| proposal.nonce);
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

    async fn delete_delta(&self, account_id: &str, nonce: u64) -> Result<(), String> {
        let path = self.get_delta_path(account_id, nonce);

        if !path.exists() {
            return Ok(()); // Already deleted or doesn't exist
        }

        fs::remove_file(&path)
            .await
            .map_err(|e| format!("Failed to delete delta file: {e}"))?;

        Ok(())
    }

    async fn update_delta_status(
        &self,
        account_id: &str,
        nonce: u64,
        status: DeltaStatus,
    ) -> Result<(), String> {
        let path = self.get_delta_path(account_id, nonce);

        let content = fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Failed to read delta file: {e}"))?;

        let mut delta: DeltaObject = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to deserialize delta: {e}"))?;

        delta.status = status;

        let updated_content = serde_json::to_string_pretty(&delta)
            .map_err(|e| format!("Failed to serialize delta: {e}"))?;

        self.write(&path, &updated_content).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta_object::{DeltaObject, DeltaStatus};
    use crate::state_object::StateObject;
    use std::env;

    fn create_test_delta(account_id: &str, nonce: u64) -> DeltaObject {
        DeltaObject {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: "0x123".to_string(),
            new_commitment: Some("0x456".to_string()),
            delta_payload: serde_json::json!({"test": "payload"}),
            ack_sig: "0xsig".to_string(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Canonical {
                timestamp: "2024-11-14T12:00:00Z".to_string(),
            },
        }
    }

    fn create_test_state(account_id: &str) -> StateObject {
        StateObject {
            account_id: account_id.to_string(),
            commitment: "0x789".to_string(),
            state_json: serde_json::json!({"test": "state"}),
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            auth_scheme: String::new(),
        }
    }

    #[tokio::test]
    async fn test_submit_and_pull_state() {
        let temp_dir = env::temp_dir().join(format!("psm_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7bfb0f38b0fafa103f86a805594170";
        let state = create_test_state(account_id);

        // Submit state
        storage
            .submit_state(&state)
            .await
            .expect("Submit state failed");

        // Pull state back
        let pulled_state = storage
            .pull_state(account_id)
            .await
            .expect("Pull state failed");

        assert_eq!(pulled_state.account_id, state.account_id);
        assert_eq!(pulled_state.commitment, state.commitment);
        assert_eq!(pulled_state.state_json, state.state_json);

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_submit_and_pull_delta() {
        let temp_dir = env::temp_dir().join(format!("psm_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7bfb0f38b0fafa103f86a805594170";
        let delta = create_test_delta(account_id, 1);

        // Submit delta
        storage
            .submit_delta(&delta)
            .await
            .expect("Submit delta failed");

        // Pull delta back
        let pulled_delta = storage
            .pull_delta(account_id, 1)
            .await
            .expect("Pull delta failed");

        assert_eq!(pulled_delta.account_id, delta.account_id);
        assert_eq!(pulled_delta.nonce, delta.nonce);
        assert_eq!(pulled_delta.delta_payload, delta.delta_payload);

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_pull_deltas_after() {
        let temp_dir = env::temp_dir().join(format!("psm_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7bfb0f38b0fafa103f86a805594170";

        // Submit multiple deltas
        for nonce in 1..=5 {
            let delta = create_test_delta(account_id, nonce);
            storage
                .submit_delta(&delta)
                .await
                .expect("Submit delta failed");
        }

        // Pull deltas after nonce 2
        let deltas = storage
            .pull_deltas_after(account_id, 2)
            .await
            .expect("Pull deltas failed");

        assert_eq!(deltas.len(), 4); // Nonces 2, 3, 4, 5
        assert_eq!(deltas[0].nonce, 2);
        assert_eq!(deltas[1].nonce, 3);
        assert_eq!(deltas[2].nonce, 4);
        assert_eq!(deltas[3].nonce, 5);

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_pull_deltas_after_empty() {
        let temp_dir = env::temp_dir().join(format!("psm_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7bfb0f38b0fafa103f86a805594170";

        // Pull deltas when none exist
        let deltas = storage
            .pull_deltas_after(account_id, 1)
            .await
            .expect("Pull deltas failed");

        assert_eq!(deltas.len(), 0);

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_submit_and_pull_delta_proposal() {
        let temp_dir = env::temp_dir().join(format!("psm_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7bfb0f38b0fafa103f86a805594170";
        let commitment = "0xabc123";
        let proposal = create_test_delta(account_id, 1);

        // Submit proposal
        storage
            .submit_delta_proposal(commitment, &proposal)
            .await
            .expect("Submit proposal failed");

        // Pull proposal back
        let pulled_proposal = storage
            .pull_delta_proposal(account_id, commitment)
            .await
            .expect("Pull proposal failed");

        assert_eq!(pulled_proposal.account_id, proposal.account_id);
        assert_eq!(pulled_proposal.nonce, proposal.nonce);

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_pull_all_delta_proposals() {
        let temp_dir = env::temp_dir().join(format!("psm_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7bfb0f38b0fafa103f86a805594170";

        // Submit multiple proposals
        let commitments = ["0xaaa", "0xbbb", "0xccc"];
        for (i, commitment) in commitments.iter().enumerate() {
            let proposal = create_test_delta(account_id, (i + 1) as u64);
            storage
                .submit_delta_proposal(commitment, &proposal)
                .await
                .expect("Submit proposal failed");
        }

        // Pull all proposals
        let proposals = storage
            .pull_all_delta_proposals(account_id)
            .await
            .expect("Pull all proposals failed");

        assert_eq!(proposals.len(), 3);

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_update_delta_proposal() {
        let temp_dir = env::temp_dir().join(format!("psm_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7bfb0f38b0fafa103f86a805594170";
        let commitment = "0xabc123";
        let mut proposal = create_test_delta(account_id, 1);

        // Submit initial proposal
        storage
            .submit_delta_proposal(commitment, &proposal)
            .await
            .expect("Submit proposal failed");

        // Update proposal
        proposal.delta_payload = serde_json::json!({"updated": true});
        storage
            .update_delta_proposal(commitment, &proposal)
            .await
            .expect("Update proposal failed");

        // Pull updated proposal
        let pulled_proposal = storage
            .pull_delta_proposal(account_id, commitment)
            .await
            .expect("Pull proposal failed");

        assert_eq!(pulled_proposal.delta_payload["updated"], true);

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_delete_delta_proposal() {
        let temp_dir = env::temp_dir().join(format!("psm_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7bfb0f38b0fafa103f86a805594170";
        let commitment = "0xabc123";
        let proposal = create_test_delta(account_id, 1);

        // Submit proposal
        storage
            .submit_delta_proposal(commitment, &proposal)
            .await
            .expect("Submit proposal failed");

        // Verify it exists
        storage
            .pull_delta_proposal(account_id, commitment)
            .await
            .expect("Pull proposal should succeed");

        // Delete proposal
        storage
            .delete_delta_proposal(account_id, commitment)
            .await
            .expect("Delete proposal failed");

        // Verify it's gone
        let result = storage.pull_delta_proposal(account_id, commitment).await;
        assert!(result.is_err(), "Pull should fail after delete");

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_delete_nonexistent_proposal() {
        let temp_dir = env::temp_dir().join(format!("psm_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7bfb0f38b0fafa103f86a805594170";
        let commitment = "0xnonexistent";

        // Delete nonexistent proposal should succeed (no-op)
        let result = storage.delete_delta_proposal(account_id, commitment).await;
        assert!(result.is_ok(), "Delete of nonexistent should succeed");

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_proposal_commitment_strip_prefix() {
        let temp_dir = env::temp_dir().join(format!("psm_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7bfb0f38b0fafa103f86a805594170";
        let commitment_with_prefix = "0xabc123";
        let commitment_without_prefix = "abc123";
        let proposal = create_test_delta(account_id, 1);

        // Submit with prefix
        storage
            .submit_delta_proposal(commitment_with_prefix, &proposal)
            .await
            .expect("Submit with prefix failed");

        // Should be able to pull with or without prefix
        let result1 = storage
            .pull_delta_proposal(account_id, commitment_with_prefix)
            .await;
        let result2 = storage
            .pull_delta_proposal(account_id, commitment_without_prefix)
            .await;

        assert!(result1.is_ok(), "Pull with prefix should work");
        assert!(result2.is_ok(), "Pull without prefix should work");

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_delete_delta() {
        let temp_dir = env::temp_dir().join(format!("psm_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7bfb0f38b0fafa103f86a805594170";
        let delta = create_test_delta(account_id, 1);

        // Submit delta
        storage
            .submit_delta(&delta)
            .await
            .expect("Submit delta failed");

        // Verify it exists
        storage
            .pull_delta(account_id, 1)
            .await
            .expect("Pull delta should succeed");

        // Delete delta
        storage
            .delete_delta(account_id, 1)
            .await
            .expect("Delete delta failed");

        // Verify it's gone
        let result = storage.pull_delta(account_id, 1).await;
        assert!(result.is_err(), "Pull should fail after delete");

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_delete_nonexistent_delta() {
        let temp_dir = env::temp_dir().join(format!("psm_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7bfb0f38b0fafa103f86a805594170";

        // Delete nonexistent delta should succeed (no-op)
        let result = storage.delete_delta(account_id, 999).await;
        assert!(result.is_ok(), "Delete of nonexistent should succeed");

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_update_delta_status() {
        let temp_dir = env::temp_dir().join(format!("psm_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7bfb0f38b0fafa103f86a805594170";
        let mut delta = create_test_delta(account_id, 1);
        delta.status = DeltaStatus::candidate("2024-01-01T00:00:00Z".to_string());

        // Submit delta as candidate
        storage
            .submit_delta(&delta)
            .await
            .expect("Submit delta failed");

        // Verify initial status
        let pulled = storage.pull_delta(account_id, 1).await.unwrap();
        assert!(pulled.status.is_candidate());
        assert_eq!(pulled.status.retry_count(), 0);

        // Update status with incremented retry
        let new_status = DeltaStatus::candidate_with_retry("2024-01-01T00:01:00Z".to_string(), 1);
        storage
            .update_delta_status(account_id, 1, new_status)
            .await
            .expect("Update status failed");

        // Verify updated status
        let pulled = storage.pull_delta(account_id, 1).await.unwrap();
        assert!(pulled.status.is_candidate());
        assert_eq!(pulled.status.retry_count(), 1);
        assert_eq!(pulled.status.timestamp(), "2024-01-01T00:01:00Z");

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }
}
