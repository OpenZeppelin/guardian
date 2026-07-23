use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::state_object::StateObject;
use crate::storage::StorageBackend;
use crate::storage::encryption::marker::{EncryptionMarker, MarkerStore};
use crate::storage::{
    AbandonIntent, AccountDeltaCursor, AccountProposalCursor, DeltaStatusCounts, DeltaStatusKind,
    GlobalDeltaCursor, GlobalDeltaRow, GlobalProposalCursor, ProposalRecord, StorageType,
};
use crate::utils::normalize_commitment_hex;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

pub struct FilesystemService {
    app_path: PathBuf,
    /// Serializes delta-status writes against the conditional candidate
    /// delete (issue #319): the filesystem has no transactions, so
    /// `delete_delta_if_candidate`'s read-check-delete and the status
    /// writes it races (`submit_delta`, `update_delta_status`) take this
    /// lock. The backend is single-process, so an in-process mutex is
    /// sufficient.
    delta_write_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
}

impl FilesystemService {
    /// Create a new FilesystemService
    pub async fn new(app_path: PathBuf) -> Result<Self, String> {
        // Validate that base directories exist or can be created
        fs::create_dir_all(&app_path)
            .await
            .map_err(|e| format!("Failed to create app directory: {e}"))?;

        Ok(Self {
            app_path,
            delta_write_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        })
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
    fn get_delta_proposal_path(
        &self,
        account_id: &str,
        commitment: &str,
    ) -> Result<PathBuf, String> {
        let normalized_commitment =
            normalize_commitment_hex(commitment).map_err(|e| e.to_string())?;
        let clean_commitment = normalized_commitment
            .strip_prefix("0x")
            .unwrap_or(&normalized_commitment);
        let proposals_dir = self.app_path.join(account_id).join("proposals");
        let path = proposals_dir.join(format!("{clean_commitment}.json"));

        if path.parent() != Some(proposals_dir.as_path()) {
            return Err(
                "Invalid commitment: resolved proposal path escapes proposals directory"
                    .to_string(),
            );
        }

        Ok(path)
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

    // ----------------------------------------------------------------------
    // Cross-account aggregate walk helpers — feature
    // `005-operator-dashboard-metrics`, FR-029.
    //
    // The filesystem backend has no global indexes, so cross-account
    // aggregates (info per-status counts, latest activity timestamp,
    // global feed walks) require fanning out across every account
    // directory. Above a configured inventory threshold we refuse to
    // perform the scan and return [`AggregateUnavailableReason::
    // FilesystemThresholdExceeded`] so callers can surface a degraded
    // marker rather than block the dashboard.
    //
    // Postgres-backed deployments do not use these helpers; they query
    // their indexes directly.
    // ----------------------------------------------------------------------

    /// Walk the per-account proposals directory and return every
    /// `(commitment, proposal)` pair that is currently in the
    /// `Pending` state. Filenames carry the commitment value
    /// (`<commitment>.json`), which the on-disk shape doesn't preserve
    /// inside the `DeltaObject` body — the new paginated methods need
    /// it for the wire `commitment` field and for the
    /// (nonce, commitment) cursor tiebreaker.
    async fn pending_proposals_with_commitment(
        &self,
        account_id: &str,
    ) -> Result<Vec<(String, DeltaObject)>, String> {
        Ok(self
            .load_proposal_records(account_id)
            .await?
            .into_iter()
            .filter(|record| record.proposal.status.is_pending())
            .map(|record| (record.commitment, record.proposal))
            .collect())
    }

    /// Walk the per-account proposals directory and return one
    /// [`ProposalRecord`] per `<commitment>.json` file, propagating any
    /// read/parse/decrypt failure so callers never see a partial list.
    async fn load_proposal_records(&self, account_id: &str) -> Result<Vec<ProposalRecord>, String> {
        let mut proposals = Vec::new();
        for filename in self.list_proposal_filenames(account_id).await? {
            let Some(commitment) = filename.strip_suffix(".json") else {
                continue;
            };
            let proposal = self.pull_delta_proposal(account_id, commitment).await?;
            proposals.push(ProposalRecord {
                account_id: account_id.to_string(),
                commitment: commitment.to_string(),
                proposal,
            });
        }
        Ok(proposals)
    }

    /// Count the number of account directories under `app_path`. An
    /// "account directory" is any immediate subdirectory of the app
    /// root. This is used by [`Self::enforce_aggregate_threshold`].
    pub async fn count_accounts(&self) -> Result<usize, String> {
        if !self.app_path.exists() {
            return Ok(0);
        }
        let mut entries = fs::read_dir(&self.app_path)
            .await
            .map_err(|e| format!("Failed to read app directory: {e}"))?;
        let mut count = 0usize;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| format!("Failed to read directory entry: {e}"))?
        {
            let file_type = entry
                .file_type()
                .await
                .map_err(|e| format!("Failed to read file type: {e}"))?;
            if file_type.is_dir() {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Returns `Ok(count)` if the on-disk inventory is at or below
    /// `threshold`; otherwise [`Err(AggregateUnavailableReason::
    /// FilesystemThresholdExceeded)`]. Service-layer callers map the
    /// error to [`crate::error::GuardianError::DataUnavailable`] when
    /// surfacing a degraded marker on the info response or returning
    /// `503` on the global feed endpoints.
    pub async fn enforce_aggregate_threshold(
        &self,
        threshold: usize,
    ) -> Result<usize, AggregateUnavailableReason> {
        let count = self
            .count_accounts()
            .await
            .map_err(AggregateUnavailableReason::CountFailed)?;
        if count > threshold {
            Err(AggregateUnavailableReason::FilesystemThresholdExceeded { count, threshold })
        } else {
            Ok(count)
        }
    }
}

/// Reason a cross-account aggregate could not be computed on the
/// filesystem backend. See FR-029 of `005-operator-dashboard-metrics`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregateUnavailableReason {
    /// On-disk inventory is above the configured threshold; the
    /// caller should mark the affected aggregate as degraded rather
    /// than perform a full scan.
    FilesystemThresholdExceeded { count: usize, threshold: usize },
    /// Counting accounts on disk failed for an underlying I/O reason.
    /// Callers should surface this as `503 DataUnavailable`.
    CountFailed(String),
}

impl AggregateUnavailableReason {
    /// Stable, machine-readable reason name for inclusion in the
    /// `degraded_aggregates` list on the info response or in the body
    /// of a `503 DataUnavailable` response.
    pub fn code(&self) -> &'static str {
        match self {
            AggregateUnavailableReason::FilesystemThresholdExceeded { .. } => {
                "filesystem_threshold_exceeded"
            }
            AggregateUnavailableReason::CountFailed(_) => "filesystem_count_failed",
        }
    }
}

impl std::fmt::Display for AggregateUnavailableReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AggregateUnavailableReason::FilesystemThresholdExceeded { count, threshold } => {
                write!(
                    f,
                    "filesystem cross-account aggregate suppressed: {count} accounts exceeds threshold {threshold}"
                )
            }
            AggregateUnavailableReason::CountFailed(msg) => {
                write!(f, "filesystem account count failed: {msg}")
            }
        }
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod aggregate_tests {
    use super::*;
    use tempfile::TempDir;

    async fn fixture_with_n_accounts(n: usize) -> (TempDir, FilesystemService) {
        let dir = TempDir::new().expect("tempdir");
        let svc = FilesystemService::new(dir.path().to_path_buf())
            .await
            .expect("filesystem service");
        for i in 0..n {
            let acc_dir = dir.path().join(format!("account_{i}"));
            fs::create_dir_all(&acc_dir).await.expect("create acc dir");
        }
        (dir, svc)
    }

    #[tokio::test]
    async fn count_accounts_empty_dir_returns_zero() {
        let (_dir, svc) = fixture_with_n_accounts(0).await;
        assert_eq!(svc.count_accounts().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn count_accounts_returns_subdir_count() {
        let (_dir, svc) = fixture_with_n_accounts(7).await;
        assert_eq!(svc.count_accounts().await.unwrap(), 7);
    }

    #[tokio::test]
    async fn count_accounts_ignores_files_at_app_root() {
        let (dir, svc) = fixture_with_n_accounts(3).await;
        // A stray file at the app root should not be counted as an
        // account.
        tokio::fs::write(dir.path().join("README.md"), "hello")
            .await
            .expect("write stray file");
        assert_eq!(svc.count_accounts().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn enforce_aggregate_threshold_below_returns_ok_with_count() {
        let (_dir, svc) = fixture_with_n_accounts(5).await;
        let count = svc.enforce_aggregate_threshold(10).await.unwrap();
        assert_eq!(count, 5);
    }

    #[tokio::test]
    async fn enforce_aggregate_threshold_at_returns_ok() {
        // At threshold is OK (we use strictly greater for the trigger).
        let (_dir, svc) = fixture_with_n_accounts(10).await;
        let count = svc.enforce_aggregate_threshold(10).await.unwrap();
        assert_eq!(count, 10);
    }

    #[tokio::test]
    async fn enforce_aggregate_threshold_above_returns_degraded() {
        let (_dir, svc) = fixture_with_n_accounts(11).await;
        let err = svc.enforce_aggregate_threshold(10).await.unwrap_err();
        match err {
            AggregateUnavailableReason::FilesystemThresholdExceeded { count, threshold } => {
                assert_eq!(count, 11);
                assert_eq!(threshold, 10);
            }
            other => panic!("expected ThresholdExceeded, got {other:?}"),
        }
    }

    #[test]
    fn aggregate_unavailable_reason_codes_are_stable() {
        let r = AggregateUnavailableReason::FilesystemThresholdExceeded {
            count: 5,
            threshold: 1,
        };
        assert_eq!(r.code(), "filesystem_threshold_exceeded");

        let r = AggregateUnavailableReason::CountFailed("io".into());
        assert_eq!(r.code(), "filesystem_count_failed");
    }
}

#[async_trait]
impl StorageBackend for FilesystemService {
    fn kind(&self) -> StorageType {
        StorageType::Filesystem
    }

    async fn submit_state(&self, state: &StateObject) -> Result<(), String> {
        let content = serde_json::to_string_pretty(state)
            .map_err(|e| format!("Failed to serialize state: {e}"))?;

        let app_path = self.get_state_path(&state.account_id);

        self.write(&app_path, &content).await
    }

    async fn submit_delta(&self, delta: &DeltaObject) -> Result<(), String> {
        let _guard = self.delta_write_lock.lock().await;
        self.write_delta_holding_lock(delta).await
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

    /// Filtered read only in what it returns: the filesystem layout has
    /// no status index, so every delta file is still opened and decoded.
    /// Acceptable for the single-process backend; the store-side win
    /// belongs to Postgres.
    async fn pull_candidate_deltas(&self, account_id: &str) -> Result<Vec<DeltaObject>, String> {
        let deltas_filenames = self.list_delta_filenames(account_id).await?;
        let mut deltas = Vec::new();

        for filename in deltas_filenames {
            if let Some(nonce_str) = filename.strip_suffix(".json")
                && let Ok(nonce) = nonce_str.parse::<u64>()
            {
                let delta = self.pull_delta(account_id, nonce).await?;
                if delta.status.is_candidate() {
                    deltas.push(delta);
                }
            }
        }

        deltas.sort_by_key(|delta| delta.nonce);
        Ok(deltas)
    }

    async fn pull_recoverable_deltas(
        &self,
        account_id: &str,
        abandoned_since: DateTime<Utc>,
    ) -> Result<Vec<DeltaObject>, String> {
        let deltas_filenames = self.list_delta_filenames(account_id).await?;
        let mut deltas = Vec::new();

        for filename in deltas_filenames {
            if let Some(nonce_str) = filename.strip_suffix(".json")
                && let Ok(nonce) = nonce_str.parse::<u64>()
            {
                let delta = self.pull_delta(account_id, nonce).await?;
                if crate::storage::is_recoverable(&delta.status, abandoned_since) {
                    deltas.push(delta);
                }
            }
        }

        deltas.sort_by_key(|delta| delta.nonce);
        Ok(deltas)
    }

    async fn list_accounts_with_recoverable_deltas(
        &self,
        abandoned_since: DateTime<Utc>,
    ) -> Result<Vec<String>, String> {
        let account_ids = self.fanout_account_ids().await?;
        let mut with_recoverable = Vec::new();
        for account_id in account_ids {
            // Per-account tolerance: one unreadable delta file must not
            // silently disable reconciliation (and TTL expiry) for every
            // other account in the store.
            match self
                .pull_recoverable_deltas(&account_id, abandoned_since)
                .await
            {
                Ok(recoverable) if !recoverable.is_empty() => with_recoverable.push(account_id),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        account_id = %account_id,
                        error = %e,
                        "Skipping account in recoverable-delta scan; \
                         its rows wait until the read heals"
                    );
                }
            }
        }
        Ok(with_recoverable)
    }

    // Delta proposal methods - stored separately from executed deltas
    async fn submit_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String> {
        let path = self.get_delta_proposal_path(&proposal.account_id, commitment)?;

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
        let path = self.get_delta_proposal_path(account_id, commitment)?;

        let json = fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Failed to read proposal file: {e}"))?;

        let proposal: DeltaObject =
            serde_json::from_str(&json).map_err(|e| format!("Failed to parse proposal: {e}"))?;

        Ok(proposal)
    }

    async fn pull_all_delta_proposals(
        &self,
        account_id: &str,
    ) -> Result<Vec<ProposalRecord>, String> {
        self.load_proposal_records(account_id).await
    }

    async fn pull_pending_proposals(
        &self,
        account_id: &str,
    ) -> Result<Vec<ProposalRecord>, String> {
        let mut proposals = self.load_proposal_records(account_id).await?;
        proposals.retain(|record| record.proposal.status.is_pending());
        proposals.sort_by_key(|record| record.proposal.nonce);
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
        let path = self.get_delta_proposal_path(account_id, commitment)?;

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

    async fn request_candidate_abandon(
        &self,
        account_id: &str,
        nonce: u64,
        now: &str,
    ) -> Result<AbandonIntent, String> {
        let path = self.get_delta_path(account_id, nonce);

        // Read-check-write under the delta write lock: status writes
        // (`submit_delta`, `update_delta_status`) take the same lock, so
        // the intent annotation can neither clobber a concurrent status
        // transition nor lose worker-owned counters.
        let _guard = self.delta_write_lock.lock().await;

        let content = match fs::read_to_string(&path).await {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AbandonIntent::NotCandidate);
            }
            Err(e) => return Err(format!("Failed to read delta file: {e}")),
        };
        let mut delta: DeltaObject = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to deserialize delta: {e}"))?;

        if !delta.status.is_candidate() {
            return Ok(AbandonIntent::NotCandidate);
        }
        if let Some(requested_at) = delta.status.abandon_requested_at() {
            return Ok(AbandonIntent::AlreadyRequested {
                requested_at: requested_at.to_string(),
            });
        }

        delta.status = delta.status.with_abandon_requested(now.to_string());
        let updated = serde_json::to_string_pretty(&delta)
            .map_err(|e| format!("Failed to serialize delta: {e}"))?;
        self.write(&path, &updated).await?;

        Ok(AbandonIntent::Recorded)
    }

    async fn update_delta_status(
        &self,
        account_id: &str,
        nonce: u64,
        status: DeltaStatus,
    ) -> Result<(), String> {
        let path = self.get_delta_path(account_id, nonce);

        let _guard = self.delta_write_lock.lock().await;
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

    // Canonicalization lifecycle writes: the filesystem backend is
    // single-process by construction (no shared coordination store), so
    // no fence applies. It is NOT single-task, though: API handlers and
    // the canonicalization worker interleave as tokio tasks in the same
    // process, and retained/abandoned rows (issue #345) are precisely
    // the rows a client submission may supersede while the worker
    // reconciles or expires them. Every read-check-act sequence below
    // therefore holds `delta_write_lock` end to end — the single-process
    // equivalent of the Postgres account-locked transaction — so a kind
    // guard checked by one task can never be invalidated by another
    // before its write lands.

    async fn submit_candidate(
        &self,
        metadata: &dyn crate::metadata::MetadataStore,
        delta: &DeltaObject,
        now: &str,
    ) -> Result<crate::storage::CandidateSubmission, String> {
        let _guard = self.delta_write_lock.lock().await;
        if let Ok(existing) = self.pull_delta(&delta.account_id, delta.nonce).await
            && (existing.status.is_retained() || existing.status.is_client_abandoned())
        {
            self.delete_delta(&delta.account_id, delta.nonce).await?;
        }
        self.write_delta_holding_lock(delta).await?;
        metadata
            .set_has_pending_candidate(&delta.account_id, true, now)
            .await?;
        Ok(crate::storage::CandidateSubmission::Submitted)
    }

    async fn promote_candidate(
        &self,
        metadata: &dyn crate::metadata::MetadataStore,
        promotion: crate::storage::CandidatePromotion,
    ) -> Result<crate::storage::PromoteWrite, String> {
        let _guard = self.delta_write_lock.lock().await;

        // Source-kind gate under the lock: a superseded row cannot be
        // stamped canonical, and a promoted row cannot be superseded
        // mid-promotion.
        if let Ok(existing) = self
            .pull_delta(&promotion.state.account_id, promotion.delta.nonce)
            .await
            && !promotion.source.matches(&existing.status)
        {
            return Ok(crate::storage::PromoteWrite::NotCandidate);
        }

        let current_state = self.pull_state(&promotion.state.account_id).await?;
        if current_state.commitment != promotion.delta.prev_commitment {
            return Ok(crate::storage::PromoteWrite::StaleBase);
        }
        self.submit_state(&promotion.state).await?;
        if let Some(new_auth) = promotion.new_auth {
            metadata
                .update_auth(&promotion.state.account_id, new_auth, &promotion.now)
                .await?;
        }
        self.write_delta_holding_lock(&promotion.delta).await?;
        metadata
            .clear_pending_candidate_if_none(&promotion.state.account_id, &promotion.now)
            .await?;
        Ok(crate::storage::PromoteWrite::Applied)
    }

    async fn discard_candidate(
        &self,
        metadata: &dyn crate::metadata::MetadataStore,
        account_id: &str,
        nonce: u64,
        kind: DeltaStatusKind,
        now: &str,
        _fence: Option<&crate::storage::LeaseFence>,
    ) -> Result<crate::storage::CanonicalWrite, String> {
        // The sequential helper only calls lock-free primitives
        // (`pull_delta`, `delete_delta`, metadata flag ops), so holding
        // the guard across it is deadlock-free.
        let _guard = self.delta_write_lock.lock().await;
        crate::storage::discard_candidate_sequential(self, metadata, account_id, nonce, kind, now)
            .await
    }

    async fn update_candidate_status(
        &self,
        account_id: &str,
        nonce: u64,
        status: DeltaStatus,
        _fence: Option<&crate::storage::LeaseFence>,
    ) -> Result<crate::storage::CanonicalWrite, String> {
        let path = self.get_delta_path(account_id, nonce);

        // Read-modify-write under the delta write lock (the same lock
        // `request_candidate_abandon` takes): the new status is computed
        // from the worker's tick-start snapshot, so a concurrently
        // recorded abandon request must be carried into the overwrite.
        let _guard = self.delta_write_lock.lock().await;

        let content = match fs::read_to_string(&path).await {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(crate::storage::CanonicalWrite::NotCandidate);
            }
            Err(e) => return Err(format!("Failed to read delta file: {e}")),
        };
        let mut delta: DeltaObject = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to deserialize delta: {e}"))?;

        if !delta.status.is_candidate() {
            return Ok(crate::storage::CanonicalWrite::NotCandidate);
        }

        // A concurrently recorded abandon intent must not be wiped into
        // a retained status, which has no field to carry it: refuse the
        // flip — the next worker tick sees the intent in its snapshot
        // and resolves the abandon instead.
        if status.is_retained() && delta.status.abandon_requested_at().is_some() {
            return Ok(crate::storage::CanonicalWrite::NotCandidate);
        }

        delta.status =
            status.with_abandon_request_preserved_from(delta.status.abandon_requested_at());
        let updated = serde_json::to_string_pretty(&delta)
            .map_err(|e| format!("Failed to serialize delta: {e}"))?;
        self.write(&path, &updated).await?;

        Ok(crate::storage::CanonicalWrite::Applied)
    }

    // ----------------------------------------------------------------------
    // Dashboard read APIs (feature `005-operator-dashboard-metrics`).
    //
    // Filesystem has no global indexes, so cross-account aggregates
    // either fan out across every account directory or refuse with
    // [`AggregateUnavailableReason::FilesystemThresholdExceeded`] when
    // above the configured inventory size. Per-account methods walk
    // one account directory and sort/slice in memory; bounded by the
    // per-account history size, which is acceptable at MVP scale.
    // ----------------------------------------------------------------------

    async fn list_account_deltas_paged(
        &self,
        account_id: &str,
        limit: u32,
        cursor: Option<AccountDeltaCursor>,
    ) -> Result<Vec<DeltaObject>, String> {
        let cutoff = cursor.map(|c| c.last_nonce as u64);
        let mut deltas: Vec<DeltaObject> = self
            .pull_deltas_after(account_id, 0)
            .await?
            .into_iter()
            .filter(|d| !matches!(d.status, DeltaStatus::Pending { .. }))
            .filter(|d| cutoff.is_none_or(|cutoff_nonce| d.nonce < cutoff_nonce))
            .collect();
        deltas.sort_by(|a, b| b.nonce.cmp(&a.nonce));
        deltas.truncate(limit as usize);
        Ok(deltas)
    }

    async fn list_account_proposals_paged(
        &self,
        account_id: &str,
        limit: u32,
        cursor: Option<AccountProposalCursor>,
    ) -> Result<Vec<ProposalRecord>, String> {
        let mut rows: Vec<ProposalRecord> = self
            .pending_proposals_with_commitment(account_id)
            .await?
            .into_iter()
            .filter(|(commitment, proposal)| match cursor.as_ref() {
                None => true,
                Some(c) => {
                    let cn = c.last_nonce as u64;
                    proposal.nonce < cn
                        || (proposal.nonce == cn
                            && commitment.as_str() < c.last_commitment.as_str())
                }
            })
            .map(|(commitment, proposal)| ProposalRecord {
                account_id: account_id.to_string(),
                commitment,
                proposal,
            })
            .collect();
        rows.sort_by(|a, b| {
            b.proposal
                .nonce
                .cmp(&a.proposal.nonce)
                .then_with(|| b.commitment.cmp(&a.commitment))
        });
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn list_global_deltas_paged(
        &self,
        limit: u32,
        cursor: Option<GlobalDeltaCursor>,
        status_filter: Option<Vec<DeltaStatusKind>>,
    ) -> Result<Vec<GlobalDeltaRow>, String> {
        let account_ids = self.fanout_account_ids().await?;
        // Hold the parsed cursor `DateTime<Utc>` so cutoff comparison
        // is instant-based, not string-based. Comparing raw RFC3339
        // strings is fragile: `2026-05-11T12:17:34Z` and
        // `2026-05-11T12:17:34.000+00:00` represent the same instant
        // but compare differently lexicographically, which can skip
        // or duplicate page boundaries.
        let cutoff = cursor.as_ref().map(|c| {
            (
                c.last_status_timestamp,
                c.last_account_id.clone(),
                c.last_nonce as u64,
            )
        });
        let mut rows: Vec<GlobalDeltaRow> = Vec::new();
        for account_id in &account_ids {
            let deltas = self.pull_deltas_after(account_id, 0).await?;
            for delta in deltas {
                let kind = match &delta.status {
                    DeltaStatus::Pending { .. } => continue,
                    status => DeltaStatusKind::of(status),
                };
                if let Some(allowed) = &status_filter
                    && !allowed.contains(&kind)
                {
                    continue;
                }
                if let Some((cutoff_ts, cutoff_account, cutoff_nonce)) = &cutoff {
                    // Unparseable timestamps sort as `MIN_UTC` so
                    // they land at the back of the DESC feed and
                    // never accidentally jump the cutoff.
                    let parsed = parse_status_timestamp(delta.status.timestamp())
                        .unwrap_or(DateTime::<Utc>::MIN_UTC);
                    let keep = match parsed.cmp(cutoff_ts) {
                        Ordering::Less => true,
                        Ordering::Greater => false,
                        Ordering::Equal => match account_id.cmp(cutoff_account) {
                            Ordering::Less => false,
                            Ordering::Greater => true,
                            Ordering::Equal => delta.nonce > *cutoff_nonce,
                        },
                    };
                    if !keep {
                        continue;
                    }
                }
                rows.push(GlobalDeltaRow {
                    account_id: account_id.clone(),
                    delta,
                });
            }
        }
        // Newest-first by parsed `DateTime<Utc>`, then account_id
        // ASC, then nonce ASC — mirrors the Postgres SQL ORDER BY.
        // Parsing on the sort path means two rows representing the
        // same instant land in the deterministic tie-break order
        // regardless of how their RFC3339 strings happen to be
        // formatted.
        rows.sort_by(|a, b| {
            let ts_a = parse_status_timestamp(a.delta.status.timestamp())
                .unwrap_or(DateTime::<Utc>::MIN_UTC);
            let ts_b = parse_status_timestamp(b.delta.status.timestamp())
                .unwrap_or(DateTime::<Utc>::MIN_UTC);
            ts_b.cmp(&ts_a)
                .then_with(|| a.account_id.cmp(&b.account_id))
                .then_with(|| a.delta.nonce.cmp(&b.delta.nonce))
        });
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn list_global_proposals_paged(
        &self,
        limit: u32,
        cursor: Option<GlobalProposalCursor>,
    ) -> Result<Vec<ProposalRecord>, String> {
        let account_ids = self.fanout_account_ids().await?;
        // See `list_global_deltas_paged` above for the rationale on
        // holding the cutoff as a parsed `DateTime<Utc>`.
        let cutoff = cursor.as_ref().map(|c| {
            (
                c.last_originating_timestamp,
                c.last_account_id.clone(),
                c.last_nonce as u64,
                c.last_commitment.clone(),
            )
        });
        let mut rows: Vec<ProposalRecord> = Vec::new();
        for account_id in &account_ids {
            for (commitment, proposal) in self.pending_proposals_with_commitment(account_id).await?
            {
                if let Some((cutoff_ts, cutoff_account, cutoff_nonce, cutoff_commitment)) = &cutoff
                {
                    let parsed = parse_status_timestamp(proposal.status.timestamp())
                        .unwrap_or(DateTime::<Utc>::MIN_UTC);
                    let keep = match parsed.cmp(cutoff_ts) {
                        Ordering::Less => true,
                        Ordering::Greater => false,
                        Ordering::Equal => match account_id.as_str().cmp(cutoff_account.as_str()) {
                            Ordering::Less => false,
                            Ordering::Greater => true,
                            Ordering::Equal => match proposal.nonce.cmp(cutoff_nonce) {
                                Ordering::Less => false,
                                Ordering::Greater => true,
                                Ordering::Equal => commitment.as_str() > cutoff_commitment.as_str(),
                            },
                        },
                    };
                    if !keep {
                        continue;
                    }
                }
                rows.push(ProposalRecord {
                    account_id: account_id.clone(),
                    commitment,
                    proposal,
                });
            }
        }
        rows.sort_by(|a, b| {
            let ts_a = parse_status_timestamp(a.proposal.status.timestamp())
                .unwrap_or(DateTime::<Utc>::MIN_UTC);
            let ts_b = parse_status_timestamp(b.proposal.status.timestamp())
                .unwrap_or(DateTime::<Utc>::MIN_UTC);
            ts_b.cmp(&ts_a)
                .then_with(|| a.account_id.cmp(&b.account_id))
                .then_with(|| a.proposal.nonce.cmp(&b.proposal.nonce))
                .then_with(|| a.commitment.cmp(&b.commitment))
        });
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn count_deltas_by_status(&self) -> Result<DeltaStatusCounts, String> {
        let account_ids = self.fanout_account_ids().await?;
        let mut counts = DeltaStatusCounts::default();
        for account_id in &account_ids {
            let deltas = self.pull_deltas_after(account_id, 0).await?;
            for delta in deltas {
                match delta.status {
                    DeltaStatus::Candidate { .. } => counts.candidate += 1,
                    DeltaStatus::Canonical { .. } => counts.canonical += 1,
                    DeltaStatus::Retained { .. } => counts.retained += 1,
                    DeltaStatus::Discarded { .. } => counts.discarded += 1,
                    DeltaStatus::Pending { .. } => {}
                }
            }
        }
        Ok(counts)
    }

    async fn count_in_flight_proposals(&self) -> Result<u64, String> {
        let account_ids = self.fanout_account_ids().await?;
        let mut total: u64 = 0;
        for account_id in &account_ids {
            let proposals = self.pull_pending_proposals(account_id).await?;
            total += proposals.len() as u64;
        }
        Ok(total)
    }

    async fn latest_activity_timestamp(&self) -> Result<Option<DateTime<Utc>>, String> {
        let account_ids = self.fanout_account_ids().await?;
        let mut latest: Option<DateTime<Utc>> = None;
        for account_id in &account_ids {
            let deltas = self.pull_deltas_after(account_id, 0).await?;
            for delta in deltas {
                if let Some(ts) = parse_status_timestamp(delta.status.timestamp()) {
                    latest = match latest {
                        None => Some(ts),
                        Some(existing) if ts > existing => Some(ts),
                        Some(existing) => Some(existing),
                    };
                }
            }
            let proposals = self.pull_pending_proposals(account_id).await?;
            for record in proposals {
                if let Some(ts) = parse_status_timestamp(record.proposal.status.timestamp()) {
                    latest = match latest {
                        None => Some(ts),
                        Some(existing) if ts > existing => Some(ts),
                        Some(existing) => Some(existing),
                    };
                }
            }
        }
        Ok(latest)
    }
}

/// Enumerate account directories under `app_path` for the cross-account
/// fan-out methods. Used by the dashboard global feed and aggregate
/// implementations.
impl FilesystemService {
    /// Serialize and write a delta row WITHOUT taking `delta_write_lock`.
    /// Callers must already hold the lock — this exists so the lifecycle
    /// writes (`submit_candidate`, `promote_candidate`) can compose the
    /// row write into a larger guarded read-check-act sequence without
    /// deadlocking on the non-reentrant mutex.
    async fn write_delta_holding_lock(&self, delta: &DeltaObject) -> Result<(), String> {
        let content = serde_json::to_string_pretty(delta)
            .map_err(|e| format!("Failed to serialize delta: {e}"))?;
        let app_path = self.get_delta_path(&delta.account_id, delta.nonce);
        self.write(&app_path, &content).await
    }

    async fn fanout_account_ids(&self) -> Result<Vec<String>, String> {
        if !self.app_path.exists() {
            return Ok(Vec::new());
        }
        let mut entries = fs::read_dir(&self.app_path)
            .await
            .map_err(|e| format!("Failed to read app directory: {e}"))?;
        let mut ids = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| format!("Failed to read directory entry: {e}"))?
        {
            let file_type = entry
                .file_type()
                .await
                .map_err(|e| format!("Failed to read file type: {e}"))?;
            if file_type.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                ids.push(name.to_string());
            }
        }
        ids.sort();
        Ok(ids)
    }
}

const ENCRYPTION_MARKER_FILE: &str = ".encryption-marker.json";

#[async_trait]
impl MarkerStore for FilesystemService {
    async fn read_encryption_marker(&self) -> Result<Option<EncryptionMarker>, String> {
        let path = self.app_path.join(ENCRYPTION_MARKER_FILE);
        match fs::read_to_string(&path).await {
            Ok(content) => serde_json::from_str(&content)
                .map(Some)
                .map_err(|e| format!("Failed to parse encryption marker: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("Failed to read encryption marker: {e}")),
        }
    }

    async fn write_encryption_marker(&self, marker: &EncryptionMarker) -> Result<(), String> {
        let content = serde_json::to_string_pretty(marker)
            .map_err(|e| format!("Failed to serialize encryption marker: {e}"))?;
        self.write(&self.app_path.join(ENCRYPTION_MARKER_FILE), &content)
            .await
    }

    async fn has_payload_records(&self) -> Result<bool, String> {
        for account_id in self.fanout_account_ids().await? {
            if self.get_state_path(&account_id).exists() {
                return Ok(true);
            }
            if !self.list_delta_filenames(&account_id).await?.is_empty() {
                return Ok(true);
            }
            if !self.list_proposal_filenames(&account_id).await?.is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn parse_status_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    if raw.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
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
            metadata: None,
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
    async fn test_pull_recoverable_deltas_filters_and_orders() {
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let canonical = create_test_delta(account_id, 1);
        let mut retained_late = create_test_delta(account_id, 3);
        retained_late.status = DeltaStatus::retained(
            "2024-11-14T12:00:00Z".to_string(),
            crate::delta_object::RetainReason::RetryExhausted,
        );
        let mut retained_early = create_test_delta(account_id, 2);
        retained_early.status = DeltaStatus::retained(
            "2024-11-14T12:00:00Z".to_string(),
            crate::delta_object::RetainReason::Diverged,
        );
        // A recent client-abandoned discard is in scope (the issue #319
        // late-landing net); one past the cutoff is not.
        let mut abandoned_recent = create_test_delta(account_id, 4);
        abandoned_recent.status =
            DeltaStatus::discarded_client_abandoned("2024-11-14T11:30:00Z".to_string());
        let mut abandoned_old = create_test_delta(account_id, 5);
        abandoned_old.status =
            DeltaStatus::discarded_client_abandoned("2024-11-10T00:00:00Z".to_string());
        for delta in [
            &canonical,
            &retained_late,
            &retained_early,
            &abandoned_recent,
            &abandoned_old,
        ] {
            storage.submit_delta(delta).await.expect("submit works");
        }

        let cutoff = chrono::DateTime::parse_from_rfc3339("2024-11-13T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let recoverable = storage
            .pull_recoverable_deltas(account_id, cutoff)
            .await
            .expect("recoverable read works");
        assert_eq!(
            recoverable.iter().map(|d| d.nonce).collect::<Vec<_>>(),
            vec![2, 3, 4],
            "all retained rows plus only the recent abandoned discard"
        );

        let accounts = storage
            .list_accounts_with_recoverable_deltas(cutoff)
            .await
            .expect("account scan works");
        assert_eq!(accounts, vec![account_id.to_string()]);

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn test_discard_kind_guard_spares_other_lifecycles() {
        // The expected-kind guard: a candidate-kind discard must never
        // delete a retained row and vice versa — a stale worker's delayed
        // discard cannot remove a row that moved on.
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");
        let metadata_store =
            crate::metadata::filesystem::FilesystemMetadataStore::new(temp_dir.clone())
                .await
                .expect("metadata store");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let mut retained = create_test_delta(account_id, 1);
        retained.status = DeltaStatus::retained(
            "2024-11-14T12:00:00Z".to_string(),
            crate::delta_object::RetainReason::RetryExhausted,
        );
        storage.submit_delta(&retained).await.expect("submit works");

        let outcome = storage
            .discard_candidate(
                &metadata_store,
                account_id,
                1,
                DeltaStatusKind::Candidate,
                "2024-11-14T12:05:00Z",
                None,
            )
            .await
            .expect("discard resolves");
        assert_eq!(outcome, crate::storage::CanonicalWrite::NotCandidate);
        assert!(
            storage.pull_delta(account_id, 1).await.is_ok(),
            "a candidate-kind discard spares the retained row"
        );

        let outcome = storage
            .discard_candidate(
                &metadata_store,
                account_id,
                1,
                DeltaStatusKind::Retained,
                "2024-11-14T12:06:00Z",
                None,
            )
            .await
            .expect("discard resolves");
        assert_eq!(outcome, crate::storage::CanonicalWrite::Applied);
        assert!(
            storage.pull_delta(account_id, 1).await.is_err(),
            "a retained-kind discard removes the retained row"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn test_submit_candidate_supersedes_retained_row() {
        // A fresh candidate at a retained row's nonce replaces it: the
        // client re-supplied its intent for that slot, and the reconcile
        // pass must never resurrect a base under the new candidate.
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");
        let metadata_store =
            crate::metadata::filesystem::FilesystemMetadataStore::new(temp_dir.clone())
                .await
                .expect("metadata store");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        crate::metadata::MetadataStore::set(
            &metadata_store,
            crate::metadata::AccountMetadata {
                account_id: account_id.to_string(),
                auth: crate::metadata::auth::Auth::MidenFalconRpo {
                    cosigner_commitments: vec![],
                },
                network_config: crate::metadata::NetworkConfig::miden_default(),
                created_at: "2024-11-14T12:00:00Z".to_string(),
                updated_at: "2024-11-14T12:00:00Z".to_string(),
                has_pending_candidate: false,
                last_auth_timestamp: None,
                paused_at: None,
                paused_reason: None,
                released_at: None,
            },
        )
        .await
        .expect("metadata seed");

        let mut retained = create_test_delta(account_id, 1);
        retained.status = DeltaStatus::retained(
            "2024-11-14T12:00:00Z".to_string(),
            crate::delta_object::RetainReason::Diverged,
        );
        storage.submit_delta(&retained).await.expect("submit works");

        let mut candidate = create_test_delta(account_id, 1);
        candidate.status = DeltaStatus::candidate("2024-11-14T12:10:00Z".to_string());
        let submission = storage
            .submit_candidate(&metadata_store, &candidate, "2024-11-14T12:10:00Z")
            .await
            .expect("submission resolves");
        assert_eq!(submission, crate::storage::CandidateSubmission::Submitted);

        let stored = storage
            .pull_delta(account_id, 1)
            .await
            .expect("row survives");
        assert!(stored.status.is_candidate(), "the candidate replaced it");

        // A client-abandoned discard at a nonce must be superseded too:
        // it is precisely the resubmission the abandon endpoint (issue
        // #319) exists to enable, and the nonce's unique constraint must
        // not refuse it.
        let mut abandoned = create_test_delta(account_id, 2);
        abandoned.status =
            DeltaStatus::discarded_client_abandoned("2024-11-14T12:20:00Z".to_string());
        storage
            .submit_delta(&abandoned)
            .await
            .expect("submit works");

        let mut rebuilt = create_test_delta(account_id, 2);
        rebuilt.status = DeltaStatus::candidate("2024-11-14T12:30:00Z".to_string());
        let submission = storage
            .submit_candidate(&metadata_store, &rebuilt, "2024-11-14T12:30:00Z")
            .await
            .expect("submission resolves");
        assert_eq!(submission, crate::storage::CandidateSubmission::Submitted);
        let stored = storage
            .pull_delta(account_id, 2)
            .await
            .expect("row survives");
        assert!(
            stored.status.is_candidate(),
            "the rebuilt candidate replaced the abandoned discard"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn test_request_candidate_abandon_records_intent() {
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let mut delta = create_test_delta(account_id, 1);
        delta.status = DeltaStatus::candidate("2024-11-14T12:00:00Z".to_string());
        storage.submit_delta(&delta).await.expect("submit works");

        let intent = storage
            .request_candidate_abandon(account_id, 1, "2024-11-14T12:05:00Z")
            .await
            .expect("intent recording works");
        assert_eq!(intent, AbandonIntent::Recorded);

        let stored = storage.pull_delta(account_id, 1).await.expect("readable");
        assert!(stored.status.is_candidate(), "status must stay candidate");
        assert_eq!(
            stored.status.abandon_requested_at(),
            Some("2024-11-14T12:05:00Z")
        );
    }

    #[tokio::test]
    async fn test_request_candidate_abandon_is_idempotent() {
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let mut delta = create_test_delta(account_id, 1);
        delta.status = DeltaStatus::candidate("2024-11-14T12:00:00Z".to_string());
        storage.submit_delta(&delta).await.expect("submit works");

        storage
            .request_candidate_abandon(account_id, 1, "2024-11-14T12:05:00Z")
            .await
            .expect("first request works");
        let retry = storage
            .request_candidate_abandon(account_id, 1, "2024-11-14T12:09:00Z")
            .await
            .expect("retry works");
        assert_eq!(
            retry,
            AbandonIntent::AlreadyRequested {
                requested_at: "2024-11-14T12:05:00Z".to_string()
            },
            "retries must preserve the original request timestamp"
        );
    }

    #[tokio::test]
    async fn test_request_candidate_abandon_spares_non_candidates() {
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        // create_test_delta is canonical by default.
        let delta = create_test_delta(account_id, 1);
        storage.submit_delta(&delta).await.expect("submit works");

        let intent = storage
            .request_candidate_abandon(account_id, 1, "2024-11-14T12:05:00Z")
            .await
            .expect("call works");
        assert_eq!(intent, AbandonIntent::NotCandidate);
        let stored = storage.pull_delta(account_id, 1).await.expect("readable");
        assert!(stored.status.is_canonical(), "canonical delta untouched");
    }

    #[tokio::test]
    async fn test_stale_counter_write_preserves_concurrent_abandon_intent() {
        // The clobber race: the worker computes a counter write from its
        // tick-start snapshot (no intent), a client records the intent in
        // between, then the worker's write lands. The stored intent must
        // survive.
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let mut delta = create_test_delta(account_id, 1);
        delta.status = DeltaStatus::candidate("2024-11-14T12:00:00Z".to_string());
        storage.submit_delta(&delta).await.expect("submit works");

        // Worker snapshot taken here (no intent yet).
        let stale_counter_write = delta.status.with_incremented_divergence();

        // Client records the intent.
        storage
            .request_candidate_abandon(account_id, 1, "2024-11-14T12:05:00Z")
            .await
            .expect("intent recording works");

        // Worker's stale write lands.
        let outcome = storage
            .update_candidate_status(account_id, 1, stale_counter_write, None)
            .await
            .expect("status update works");
        assert_eq!(outcome, crate::storage::CanonicalWrite::Applied);

        let stored = storage.pull_delta(account_id, 1).await.expect("readable");
        assert_eq!(
            stored.status.abandon_requested_at(),
            Some("2024-11-14T12:05:00Z"),
            "the concurrently recorded intent must survive the counter write"
        );
        assert_eq!(stored.status.divergence_count(), 1);
    }

    #[tokio::test]
    async fn test_update_candidate_status_spares_non_candidates() {
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        // create_test_delta is canonical by default.
        let delta = create_test_delta(account_id, 1);
        storage.submit_delta(&delta).await.expect("submit works");

        let outcome = storage
            .update_candidate_status(
                account_id,
                1,
                DeltaStatus::candidate("2024-11-14T12:00:00Z".to_string()),
                None,
            )
            .await
            .expect("call works");
        assert_eq!(outcome, crate::storage::CanonicalWrite::NotCandidate);
        let stored = storage.pull_delta(account_id, 1).await.expect("readable");
        assert!(stored.status.is_canonical(), "canonical delta untouched");
    }

    #[tokio::test]
    async fn test_request_candidate_abandon_missing_is_not_candidate() {
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let intent = storage
            .request_candidate_abandon("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b", 1, "now")
            .await
            .expect("call works");
        assert_eq!(intent, AbandonIntent::NotCandidate);
    }

    #[tokio::test]
    async fn test_submit_and_pull_state() {
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
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
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
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
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";

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
    async fn test_pull_candidate_deltas_filters_and_orders() {
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";

        // Mixed history: canonical at 1 and 3, candidates at 4 and 2
        // (submitted out of nonce order).
        for nonce in [1u64, 3] {
            storage
                .submit_delta(&create_test_delta(account_id, nonce))
                .await
                .expect("Submit delta failed");
        }
        for nonce in [4u64, 2] {
            let mut delta = create_test_delta(account_id, nonce);
            delta.status = DeltaStatus::candidate("2024-11-14T12:00:00Z".to_string());
            storage
                .submit_delta(&delta)
                .await
                .expect("Submit delta failed");
        }

        let candidates = storage
            .pull_candidate_deltas(account_id)
            .await
            .expect("Pull candidate deltas failed");

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].nonce, 2);
        assert_eq!(candidates[1].nonce, 4);
        assert!(candidates.iter().all(|d| d.status.is_candidate()));

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_pull_deltas_after_empty() {
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";

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
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
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
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";

        // Submit multiple proposals
        let commitments = ["0xaaaa", "0xbbbb", "0xcccc"];
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
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
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
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
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
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let commitment = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        // Delete nonexistent proposal should succeed (no-op)
        let result = storage.delete_delta_proposal(account_id, commitment).await;
        assert!(result.is_ok(), "Delete of nonexistent should succeed");

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_proposal_commitment_strip_prefix() {
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
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
    async fn test_proposal_commitment_rejects_path_traversal() {
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let result = storage
            .pull_delta_proposal(account_id, "../../other_account/proposals/abc")
            .await;

        assert!(result.is_err(), "Traversal commitment should be rejected");
        assert!(result.unwrap_err().contains("Invalid commitment"));

        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_delete_delta() {
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
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
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";

        // Delete nonexistent delta should succeed (no-op)
        let result = storage.delete_delta(account_id, 999).await;
        assert!(result.is_ok(), "Delete of nonexistent should succeed");

        // Cleanup
        tokio::fs::remove_dir_all(temp_dir).await.ok();
    }

    #[tokio::test]
    async fn test_update_delta_status() {
        let temp_dir = env::temp_dir().join(format!("guardian_test_{}", uuid::Uuid::new_v4()));
        let storage = FilesystemService::new(temp_dir.clone())
            .await
            .expect("Failed to create storage");

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
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
