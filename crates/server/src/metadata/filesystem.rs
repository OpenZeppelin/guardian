use crate::metadata::{AccountListCursor, AccountMetadata, Auth, MetadataStore};
use crate::services::account_status::{AccountStatus, PauseTransition};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Filesystem-based metadata store
/// Stores all account metadata in a single JSON file with in-memory cache
pub struct FilesystemMetadataStore {
    file_path: PathBuf,
    /// In-memory cache of account metadata
    cache: Arc<RwLock<HashMap<String, AccountMetadata>>>,
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
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_path = self.file_path.with_extension(format!(
            "tmp.{}.{}.{}",
            std::process::id(),
            nanos,
            counter
        ));
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

    async fn list_paged(
        &self,
        limit: u32,
        cursor: Option<AccountListCursor>,
    ) -> Result<Vec<AccountMetadata>, String> {
        let cache = self.cache.read().await;
        let cutoff = cursor.map(|c| (c.last_updated_at, c.last_account_id));
        let mut rows: Vec<AccountMetadata> = cache
            .values()
            .filter(|m| match &cutoff {
                None => true,
                Some((cutoff_ts, cutoff_id)) => {
                    let parsed = chrono::DateTime::parse_from_rfc3339(&m.updated_at)
                        .ok()
                        .map(|dt| dt.with_timezone(&chrono::Utc));
                    match parsed {
                        Some(ts) => {
                            ts < *cutoff_ts || (ts == *cutoff_ts && m.account_id > *cutoff_id)
                        }
                        // If updated_at can't be parsed, drop the row
                        // from the cursor walk rather than risk
                        // misordering (matches the spec's stable
                        // contract for well-formed timestamps).
                        None => false,
                    }
                }
            })
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            // Newest-first by updated_at, then account_id ASC.
            let ats = chrono::DateTime::parse_from_rfc3339(&a.updated_at).ok();
            let bts = chrono::DateTime::parse_from_rfc3339(&b.updated_at).ok();
            bts.cmp(&ats).then_with(|| a.account_id.cmp(&b.account_id))
        });
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn list_with_pending_candidates(&self) -> Result<Vec<String>, String> {
        let cache = self.cache.read().await;
        Ok(cache
            .iter()
            .filter(|(_, m)| m.has_pending_candidate)
            .map(|(k, _)| k.clone())
            .collect())
    }

    async fn update_last_auth_timestamp_cas(
        &self,
        account_id: &str,
        new_timestamp: i64,
        now: &str,
    ) -> Result<bool, String> {
        let mut cache = self.cache.write().await;

        let metadata = cache
            .get_mut(account_id)
            .ok_or_else(|| format!("Account not found: {account_id}"))?;

        if let Some(current) = metadata.last_auth_timestamp
            && new_timestamp <= current
        {
            return Ok(false); // Potential replay, don't update
        }

        metadata.last_auth_timestamp = Some(new_timestamp);
        metadata.updated_at = now.to_string();

        self.persist(&cache).await?;
        Ok(true)
    }

    /// First-writer-wins pause: re-pause leaves the original
    /// `paused_at`/`paused_reason` intact. Serialized through the
    /// existing in-memory write lock.
    async fn set_pause(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
        reason: &str,
    ) -> Result<PauseTransition, String> {
        let mut cache = self.cache.write().await;
        let metadata = cache
            .get_mut(account_id)
            .ok_or_else(|| format!("Account not found: {account_id}"))?;

        let was_paused = metadata.paused_at.is_some();
        if !was_paused {
            metadata.paused_at = Some(now);
            metadata.paused_reason = Some(reason.to_string());
        }
        let transition = PauseTransition {
            before_state: if was_paused {
                AccountStatus::Paused
            } else {
                AccountStatus::Active
            },
            after_state: AccountStatus::Paused,
            paused_at: metadata.paused_at,
            paused_reason: metadata.paused_reason.clone(),
        };

        self.persist(&cache).await?;
        Ok(transition)
    }

    async fn clear_pause(&self, account_id: &str) -> Result<PauseTransition, String> {
        let mut cache = self.cache.write().await;
        let metadata = cache
            .get_mut(account_id)
            .ok_or_else(|| format!("Account not found: {account_id}"))?;

        let was_paused = metadata.paused_at.is_some();
        metadata.paused_at = None;
        metadata.paused_reason = None;
        let transition = PauseTransition {
            before_state: if was_paused {
                AccountStatus::Paused
            } else {
                AccountStatus::Active
            },
            after_state: AccountStatus::Active,
            paused_at: None,
            paused_reason: None,
        };

        self.persist(&cache).await?;
        Ok(transition)
    }

    async fn find_by_cosigner_commitment(&self, commitment: &str) -> Result<Vec<String>, String> {
        let cache = self.cache.read().await;
        let mut matches = Vec::new();
        for (account_id, metadata) in cache.iter() {
            let commitments = match &metadata.auth {
                Auth::MidenFalconRpo {
                    cosigner_commitments,
                }
                | Auth::MidenEcdsa {
                    cosigner_commitments,
                } => cosigner_commitments.as_slice(),
                // EVM accounts use a different authorization model and must
                // never appear in lookup results.
                Auth::EvmEcdsa { .. } => continue,
            };
            if commitments.iter().any(|c| c == commitment) {
                matches.push(account_id.clone());
            }
        }
        Ok(matches)
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod pause_tests {
    use super::*;
    use crate::metadata::{Auth, NetworkConfig};
    use chrono::TimeZone;

    async fn fresh_store() -> (FilesystemMetadataStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemMetadataStore::new(dir.path().to_path_buf())
            .await
            .unwrap();
        store
            .set(AccountMetadata {
                account_id: "acct".into(),
                auth: Auth::MidenFalconRpo {
                    cosigner_commitments: vec![],
                },
                network_config: NetworkConfig::Miden {
                    network_type: crate::metadata::network::MidenNetworkType::Testnet,
                },
                created_at: "2026-05-19T10:00:00Z".into(),
                updated_at: "2026-05-19T10:00:00Z".into(),
                has_pending_candidate: false,
                last_auth_timestamp: None,
                paused_at: None,
                paused_reason: None,
            })
            .await
            .unwrap();
        (store, dir)
    }

    #[tokio::test]
    async fn set_pause_is_first_writer_wins() {
        let (store, _dir) = fresh_store().await;
        let first = Utc.with_ymd_and_hms(2026, 5, 19, 14, 0, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 5, 19, 15, 30, 0).unwrap();

        let t1 = store.set_pause("acct", first, "incident A").await.unwrap();
        assert_eq!(t1.before_state, AccountStatus::Active);
        assert_eq!(t1.after_state, AccountStatus::Paused);
        assert_eq!(t1.paused_at, Some(first));
        assert_eq!(t1.paused_reason.as_deref(), Some("incident A"));

        // Re-pause: original timestamp + reason preserved.
        let t2 = store.set_pause("acct", later, "incident B").await.unwrap();
        assert_eq!(t2.before_state, AccountStatus::Paused);
        assert_eq!(t2.after_state, AccountStatus::Paused);
        assert_eq!(t2.paused_at, Some(first), "original paused_at preserved");
        assert_eq!(
            t2.paused_reason.as_deref(),
            Some("incident A"),
            "original reason preserved"
        );
    }

    #[tokio::test]
    async fn clear_pause_is_idempotent_on_active_account() {
        let (store, _dir) = fresh_store().await;
        let transition = store.clear_pause("acct").await.unwrap();
        assert_eq!(transition.before_state, AccountStatus::Active);
        assert_eq!(transition.after_state, AccountStatus::Active);
        assert!(transition.paused_at.is_none());
        assert!(transition.paused_reason.is_none());
    }

    #[tokio::test]
    async fn pause_then_clear_round_trip() {
        let (store, _dir) = fresh_store().await;
        let ts = Utc.with_ymd_and_hms(2026, 5, 19, 14, 0, 0).unwrap();
        store.set_pause("acct", ts, "compromise").await.unwrap();

        let t = store.clear_pause("acct").await.unwrap();
        assert_eq!(t.before_state, AccountStatus::Paused);
        assert_eq!(t.after_state, AccountStatus::Active);

        let post = store.get("acct").await.unwrap().unwrap();
        assert!(post.paused_at.is_none());
        assert!(post.paused_reason.is_none());
    }
}
