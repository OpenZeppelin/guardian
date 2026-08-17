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
/// Stores all account metadata in a single JSON file with in-memory cache;
/// replay-protection timestamps live in their own `auth_state.json` so an
/// authenticated read never rewrites the metadata file.
pub struct FilesystemMetadataStore {
    file_path: PathBuf,
    auth_state_path: PathBuf,
    /// In-memory cache of account metadata
    cache: Arc<RwLock<HashMap<String, AccountMetadata>>>,
    auth_state: Arc<RwLock<HashMap<String, i64>>>,
}

impl FilesystemMetadataStore {
    /// Create a new FilesystemMetadataStore
    pub async fn new(base_path: PathBuf) -> Result<Self, String> {
        let metadata_dir = base_path.join(".metadata");
        fs::create_dir_all(&metadata_dir)
            .await
            .map_err(|e| format!("Failed to create metadata directory: {e}"))?;

        let file_path = metadata_dir.join("accounts.json");
        let auth_state_path = metadata_dir.join("auth_state.json");

        let (accounts, legacy) = if file_path.exists() {
            let content = fs::read_to_string(&file_path)
                .await
                .map_err(|e| format!("Failed to read metadata file: {e}"))?;

            let accounts: HashMap<String, AccountMetadata> = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse metadata file: {e}"))?;
            let legacy = legacy_auth_state(&content)?;

            (accounts, legacy)
        } else {
            (HashMap::new(), LegacyAuthState::default())
        };
        let keys_present = legacy.keys_present;

        let auth_state = if auth_state_path.exists() {
            let content = fs::read_to_string(&auth_state_path)
                .await
                .map_err(|e| format!("Failed to read auth state file: {e}"))?;
            let mut state: HashMap<String, i64> = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse auth state file: {e}"))?;
            if keys_present {
                for (account_id, legacy_ts) in legacy.values {
                    match state.get(&account_id) {
                        Some(current) if *current >= legacy_ts => {}
                        _ => {
                            state.insert(account_id, legacy_ts);
                        }
                    }
                }
                let content = serde_json::to_string_pretty(&state)
                    .map_err(|e| format!("Failed to serialize auth state: {e}"))?;
                write_atomic(&auth_state_path, &content).await?;
            }
            state
        } else if accounts.is_empty() || keys_present {
            if !accounts.is_empty() {
                tracing::warn!(
                    accounts = accounts.len(),
                    seeded = legacy.values.len(),
                    "auth state file missing; initializing replay state from legacy metadata values"
                );
            }
            let content = serde_json::to_string_pretty(&legacy.values)
                .map_err(|e| format!("Failed to serialize auth state: {e}"))?;
            write_atomic(&auth_state_path, &content).await?;
            legacy.values
        } else {
            return Err(format!(
                "Replay-protection state file {} is missing but the metadata store \
                 already migrated off legacy timestamps; starting with empty replay \
                 state would re-accept previously seen requests. Restore \
                 auth_state.json from backup, or recreate it as an empty JSON \
                 object ({{}}) to explicitly accept that risk.",
                auth_state_path.display()
            ));
        };

        let (auth_state, expanded) = expand_account_scoped_auth_state(&accounts, auth_state)?;
        if expanded {
            let content = serde_json::to_string_pretty(&auth_state)
                .map_err(|e| format!("Failed to serialize auth state: {e}"))?;
            write_atomic(&auth_state_path, &content).await?;
        }

        let store = Self {
            file_path,
            auth_state_path,
            cache: Arc::new(RwLock::new(accounts)),
            auth_state: Arc::new(RwLock::new(auth_state)),
        };

        if keys_present {
            let cache = store.cache.read().await;
            store.persist(&cache).await?;
        }

        Ok(store)
    }

    /// Persist metadata cache to disk
    async fn persist(&self, cache: &HashMap<String, AccountMetadata>) -> Result<(), String> {
        let content = serde_json::to_string_pretty(cache)
            .map_err(|e| format!("Failed to serialize metadata: {e}"))?;
        write_atomic(&self.file_path, &content).await
    }

    async fn persist_auth_state(&self, auth_state: &HashMap<String, i64>) -> Result<(), String> {
        let content = serde_json::to_string_pretty(auth_state)
            .map_err(|e| format!("Failed to serialize auth state: {e}"))?;
        write_atomic(&self.auth_state_path, &content).await
    }
}

/// Separator for the composite `{account_id}:{signer_commitment}` replay-state
/// key. `:` cannot appear in hex account IDs or signer commitments, so the two
/// segments cannot collide, and a key without it is recognizably account-scoped
/// (pre-#367).
const AUTH_STATE_KEY_SEPARATOR: char = ':';

fn auth_state_key(account_id: &str, signer_commitment: &str) -> String {
    format!("{account_id}{AUTH_STATE_KEY_SEPARATOR}{signer_commitment}")
}

/// Replay state was account-scoped before issue #367. An account-scoped entry
/// (key without the separator) is expanded to one per-signer entry for every
/// currently authorized commitment (preserving the replay floor across the
/// upgrade instead of re-accepting requests seen just before it), and the
/// account-scoped entry is dropped. Entries for accounts that no longer exist
/// are dropped with it.
fn expand_account_scoped_auth_state(
    accounts: &HashMap<String, AccountMetadata>,
    state: HashMap<String, i64>,
) -> Result<(HashMap<String, i64>, bool), String> {
    let mut expanded = HashMap::with_capacity(state.len());
    let mut changed = false;
    for (key, timestamp) in state {
        if key.contains(AUTH_STATE_KEY_SEPARATOR) {
            merge_auth_timestamp(&mut expanded, key, timestamp);
            continue;
        }
        changed = true;
        let Some(metadata) = accounts.get(&key) else {
            continue;
        };
        validate_auth_state_signers(&key, &metadata.auth)?;
        for commitment in metadata.auth.cosigner_commitments() {
            merge_auth_timestamp(&mut expanded, auth_state_key(&key, commitment), timestamp);
        }
    }
    Ok((expanded, changed))
}

fn merge_auth_timestamp(state: &mut HashMap<String, i64>, key: String, timestamp: i64) {
    let stored = state.entry(key).or_insert(timestamp);
    *stored = (*stored).max(timestamp);
}

fn validate_auth_state_signers(account_id: &str, auth: &Auth) -> Result<(), String> {
    let commitments = auth.cosigner_commitments();
    let hex_length = match auth {
        Auth::EvmEcdsa { .. } => 40,
        Auth::MidenFalconRpo { .. } | Auth::MidenEcdsa { .. } => 64,
    };
    let invalid = commitments.is_empty()
        || commitments.iter().any(|commitment| {
            let hex = commitment.strip_prefix("0x").unwrap_or("");
            hex.len() != hex_length
                || !hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
    if invalid {
        return Err(format!(
            "Replay state for account {account_id} cannot be expanded because its auth metadata \
             has no canonical authorized signer set"
        ));
    }
    Ok(())
}

/// Replay timestamps recorded by pre-split servers inside the metadata file.
/// Pre-split `AccountMetadata` always serialized the `last_auth_timestamp`
/// key (as `null` when never authenticated), so `keys_present` distinguishes
/// a pre-split store awaiting its first migration from a post-split store —
/// the marker that lets startup fail closed when `auth_state.json` disappears
/// after migration instead of silently accepting replays with empty state.
#[derive(Default)]
struct LegacyAuthState {
    values: HashMap<String, i64>,
    keys_present: bool,
}

fn legacy_auth_state(metadata_file_content: &str) -> Result<LegacyAuthState, String> {
    let raw: serde_json::Value = serde_json::from_str(metadata_file_content)
        .map_err(|e| format!("Failed to parse metadata file: {e}"))?;
    let entries = raw
        .as_object()
        .ok_or_else(|| "Metadata file is not a JSON object".to_string())?;

    let mut legacy = LegacyAuthState::default();
    for (account_id, metadata) in entries {
        if let Some(timestamp) = metadata.get("last_auth_timestamp") {
            legacy.keys_present = true;
            if let Some(ts) = timestamp.as_i64() {
                legacy.values.insert(account_id.clone(), ts);
            }
        }
    }
    Ok(legacy)
}

async fn write_atomic(path: &PathBuf, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create metadata directory: {e}"))?;
    }

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_path =
        path.with_extension(format!("tmp.{}.{}.{}", std::process::id(), nanos, counter));
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

    fs::rename(&temp_path, path)
        .await
        .map_err(|e| format!("Failed to rename temp file: {e}"))?;

    Ok(())
}

#[async_trait]
impl MetadataStore for FilesystemMetadataStore {
    async fn get(&self, account_id: &str) -> Result<Option<AccountMetadata>, String> {
        let cache = self.cache.read().await;
        Ok(cache.get(account_id).cloned())
    }

    async fn set(&self, metadata: AccountMetadata) -> Result<(), String> {
        let account_id = metadata.account_id.clone();

        // Mirror the Postgres `set` semantics: pause and released state
        // are owned by `set_pause` / `clear_pause` and `set_released` /
        // `clear_released` and must not be changed by a generic metadata
        // write (e.g. reconfigure, EVM re-register).
        let mut metadata = metadata;
        {
            let mut cache = self.cache.write().await;
            if let Some(existing) = cache.get(&account_id) {
                metadata.paused_at = existing.paused_at;
                metadata.paused_reason = existing.paused_reason.clone();
                metadata.released_at = existing.released_at;
            }
            cache.insert(account_id, metadata);
        }

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
        paused: Option<bool>,
    ) -> Result<Vec<AccountMetadata>, String> {
        let cache = self.cache.read().await;
        let cutoff = cursor.map(|c| (c.last_updated_at, c.last_account_id));
        let mut rows: Vec<AccountMetadata> = cache
            .values()
            .filter(|m| match paused {
                Some(true) => m.paused_at.is_some(),
                Some(false) => m.paused_at.is_none(),
                None => true,
            })
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
        signer_commitment: &str,
        new_timestamp: i64,
    ) -> Result<bool, String> {
        // Advisory only: the cache lock is released before the auth-state
        // lock is taken, so this check does not serialize against metadata
        // writes. Safe while accounts cannot be deleted (callers resolve the
        // account before authenticating); a delete operation would have to
        // hold both locks or re-check under `auth_state`.
        {
            let cache = self.cache.read().await;
            if !cache.contains_key(account_id) {
                return Err(format!("Account not found: {account_id}"));
            }
        }

        let key = auth_state_key(account_id, signer_commitment);
        let mut auth_state = self.auth_state.write().await;

        // One probe covers the replay check, the swap, and the prior value the
        // rollback below needs; only a first-ever record allocates a key.
        let previous = match auth_state.get_mut(&key) {
            Some(current) => {
                if new_timestamp <= *current {
                    return Ok(false);
                }
                Some(std::mem::replace(current, new_timestamp))
            }
            None => {
                auth_state.insert(key.clone(), new_timestamp);
                None
            }
        };

        if let Err(persist_error) = self.persist_auth_state(&auth_state).await {
            match previous {
                Some(prior) => auth_state.insert(key, prior),
                None => auth_state.remove(&key),
            };
            return Err(persist_error);
        }
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

    async fn set_released(&self, account_id: &str, now: DateTime<Utc>) -> Result<bool, String> {
        let mut cache = self.cache.write().await;
        let metadata = cache
            .get_mut(account_id)
            .ok_or_else(|| format!("Account not found: {account_id}"))?;

        // First-writer-wins: keep the original released_at.
        if metadata.released_at.is_some() {
            return Ok(false);
        }
        metadata.released_at = Some(now);

        self.persist(&cache).await?;
        Ok(true)
    }

    async fn clear_released(&self, account_id: &str) -> Result<(), String> {
        let mut cache = self.cache.write().await;
        let metadata = cache
            .get_mut(account_id)
            .ok_or_else(|| format!("Account not found: {account_id}"))?;

        if metadata.released_at.is_none() {
            return Ok(());
        }
        metadata.released_at = None;

        self.persist(&cache).await
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
                paused_at: None,
                paused_reason: None,
                released_at: None,
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

    #[tokio::test]
    async fn set_released_is_first_writer_wins() {
        let (store, _dir) = fresh_store().await;
        let first = Utc.with_ymd_and_hms(2026, 7, 6, 10, 0, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 7, 6, 11, 0, 0).unwrap();

        assert!(store.set_released("acct", first).await.unwrap());
        // Second release is a no-op that reports "not newly released".
        assert!(!store.set_released("acct", later).await.unwrap());

        let post = store.get("acct").await.unwrap().unwrap();
        assert_eq!(
            post.released_at,
            Some(first),
            "original released_at preserved"
        );
    }

    #[tokio::test]
    async fn set_released_missing_account_errors() {
        let (store, _dir) = fresh_store().await;
        let ts = Utc.with_ymd_and_hms(2026, 7, 6, 10, 0, 0).unwrap();
        assert!(store.set_released("missing", ts).await.is_err());
    }

    #[tokio::test]
    async fn clear_released_reactivates_and_is_idempotent() {
        let (store, _dir) = fresh_store().await;
        let ts = Utc.with_ymd_and_hms(2026, 7, 6, 10, 0, 0).unwrap();
        store.set_released("acct", ts).await.unwrap();

        store.clear_released("acct").await.unwrap();
        let post = store.get("acct").await.unwrap().unwrap();
        assert!(post.released_at.is_none());

        // Idempotent on an already-active account.
        store.clear_released("acct").await.unwrap();
    }

    #[tokio::test]
    async fn generic_set_preserves_released_state() {
        let (store, _dir) = fresh_store().await;
        let ts = Utc.with_ymd_and_hms(2026, 7, 6, 10, 0, 0).unwrap();
        store.set_released("acct", ts).await.unwrap();

        // A generic metadata write (reconfigure-style) that carries
        // released_at: None must NOT clear the released state — only
        // clear_released may.
        let mut refreshed = store.get("acct").await.unwrap().unwrap();
        refreshed.released_at = None;
        refreshed.updated_at = "2026-07-06T12:00:00Z".into();
        store.set(refreshed).await.unwrap();

        let post = store.get("acct").await.unwrap().unwrap();
        assert_eq!(
            post.released_at,
            Some(ts),
            "generic set must not clear released_at"
        );
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod auth_state_tests {
    use super::*;
    use crate::metadata::{Auth, NetworkConfig};

    const SIGNER_A: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SIGNER_B: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn sample_metadata(account_id: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.into(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![SIGNER_A.into(), SIGNER_B.into()],
            },
            network_config: NetworkConfig::Miden {
                network_type: crate::metadata::network::MidenNetworkType::Testnet,
            },
            created_at: "2026-05-19T10:00:00Z".into(),
            updated_at: "2026-05-19T10:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    async fn store_with_account(dir: &tempfile::TempDir) -> FilesystemMetadataStore {
        let store = FilesystemMetadataStore::new(dir.path().to_path_buf())
            .await
            .unwrap();
        store.set(sample_metadata("acct")).await.unwrap();
        store
    }

    fn auth_state_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join(".metadata").join("auth_state.json")
    }

    fn accounts_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join(".metadata").join("accounts.json")
    }

    fn persisted_auth_state(dir: &tempfile::TempDir) -> HashMap<String, i64> {
        let content = std::fs::read_to_string(auth_state_path(dir)).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    #[tokio::test]
    async fn cas_records_only_strictly_increasing_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_account(&dir).await;

        assert!(
            store
                .update_last_auth_timestamp_cas("acct", "0xaa", 100)
                .await
                .unwrap()
        );
        assert!(
            !store
                .update_last_auth_timestamp_cas("acct", "0xaa", 100)
                .await
                .unwrap()
        );
        assert!(
            !store
                .update_last_auth_timestamp_cas("acct", "0xaa", 99)
                .await
                .unwrap()
        );
        assert_eq!(
            persisted_auth_state(&dir).get("acct:0xaa"),
            Some(&100),
            "rejected timestamps must not change the stored value"
        );

        assert!(
            store
                .update_last_auth_timestamp_cas("acct", "0xaa", 101)
                .await
                .unwrap()
        );
        assert_eq!(persisted_auth_state(&dir).get("acct:0xaa"), Some(&101));
    }

    #[tokio::test]
    async fn cas_is_scoped_per_signer_commitment() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_account(&dir).await;

        assert!(
            store
                .update_last_auth_timestamp_cas("acct", "0xaa", 100)
                .await
                .unwrap()
        );
        assert!(
            store
                .update_last_auth_timestamp_cas("acct", "0xbb", 50)
                .await
                .unwrap(),
            "another signer's newer timestamp must not lock this signer out"
        );
        assert!(
            !store
                .update_last_auth_timestamp_cas("acct", "0xbb", 50)
                .await
                .unwrap(),
            "a replay from the same signer must still be rejected"
        );
        assert_eq!(persisted_auth_state(&dir).get("acct:0xaa"), Some(&100));
        assert_eq!(persisted_auth_state(&dir).get("acct:0xbb"), Some(&50));
    }

    #[tokio::test]
    async fn account_scoped_auth_state_expands_to_per_signer_on_startup() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _store = store_with_account(&dir).await;
        }
        std::fs::write(
            auth_state_path(&dir),
            serde_json::to_string(&HashMap::from([("acct".to_string(), 4242_i64)])).unwrap(),
        )
        .unwrap();

        let store = FilesystemMetadataStore::new(dir.path().to_path_buf())
            .await
            .unwrap();

        let persisted = persisted_auth_state(&dir);
        assert_eq!(
            persisted.get(&auth_state_key("acct", SIGNER_A)),
            Some(&4242)
        );
        assert_eq!(
            persisted.get(&auth_state_key("acct", SIGNER_B)),
            Some(&4242)
        );
        assert!(
            !persisted.contains_key("acct"),
            "the account-scoped entry must be dropped after expansion"
        );
        assert!(
            !store
                .update_last_auth_timestamp_cas("acct", SIGNER_B, 4242)
                .await
                .unwrap(),
            "the pre-upgrade floor must still reject replays for every signer"
        );
    }

    #[tokio::test]
    async fn account_scoped_auth_state_with_unusable_signers_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _store = store_with_account(&dir).await;
        }
        let mut accounts: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(accounts_path(&dir)).unwrap()).unwrap();
        accounts["acct"]["auth"]["MidenFalconRpo"]["cosigner_commitments"] = serde_json::json!([]);
        std::fs::write(
            accounts_path(&dir),
            serde_json::to_string(&accounts).unwrap(),
        )
        .unwrap();
        std::fs::write(
            auth_state_path(&dir),
            serde_json::to_string(&HashMap::from([("acct".to_string(), 4242_i64)])).unwrap(),
        )
        .unwrap();

        let error = FilesystemMetadataStore::new(dir.path().to_path_buf())
            .await
            .err()
            .expect("an account-scoped floor without usable signers must reject startup");

        assert!(error.contains("no canonical authorized signer set"));
        assert_eq!(persisted_auth_state(&dir).get("acct"), Some(&4242));
    }

    #[tokio::test]
    async fn mixed_auth_state_keys_keep_the_highest_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _store = store_with_account(&dir).await;
        }
        let signer_key = auth_state_key("acct", SIGNER_A);
        std::fs::write(
            auth_state_path(&dir),
            serde_json::to_string(&HashMap::from([
                ("acct".to_string(), 5000_i64),
                (signer_key.clone(), 100_i64),
            ]))
            .unwrap(),
        )
        .unwrap();

        let _store = FilesystemMetadataStore::new(dir.path().to_path_buf())
            .await
            .unwrap();

        assert_eq!(persisted_auth_state(&dir).get(&signer_key), Some(&5000));
    }

    #[tokio::test]
    async fn cas_for_unknown_account_is_a_storage_error_not_a_replay() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_account(&dir).await;

        let err = store
            .update_last_auth_timestamp_cas("ghost", "0xaa", 100)
            .await
            .expect_err("unknown account must error");
        assert!(err.contains("Account not found"));
    }

    #[tokio::test]
    async fn cas_does_not_touch_account_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_account(&dir).await;
        let before = store.get("acct").await.unwrap().unwrap();

        assert!(
            store
                .update_last_auth_timestamp_cas("acct", "0xaa", 100)
                .await
                .unwrap()
        );

        let after = store.get("acct").await.unwrap().unwrap();
        assert_eq!(after.updated_at, before.updated_at);
        assert!(
            !std::fs::read_to_string(accounts_path(&dir))
                .unwrap()
                .contains("last_auth_timestamp")
        );
    }

    #[tokio::test]
    async fn non_auth_mutations_still_advance_updated_at() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_account(&dir).await;
        let before = store.get("acct").await.unwrap().unwrap();

        assert!(
            store
                .update_last_auth_timestamp_cas("acct", "0xaa", 100)
                .await
                .unwrap()
        );
        store
            .update_auth(
                "acct",
                Auth::MidenFalconRpo {
                    cosigner_commitments: vec!["0xc0".into()],
                },
                "2026-07-31T12:00:00Z",
            )
            .await
            .unwrap();

        let after = store.get("acct").await.unwrap().unwrap();
        assert_ne!(
            after.updated_at, before.updated_at,
            "configuration changes must advance updated_at"
        );
    }

    #[tokio::test]
    async fn concurrent_identical_timestamps_admit_exactly_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(store_with_account(&dir).await);

        let attempts = (0..16).map(|_| {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .update_last_auth_timestamp_cas("acct", "0xaa", 500)
                    .await
            })
        });
        let mut accepted = 0;
        for attempt in attempts {
            if attempt.await.unwrap().unwrap() {
                accepted += 1;
            }
        }
        assert_eq!(accepted, 1, "exactly one concurrent request may win");
    }

    #[tokio::test]
    async fn fresh_store_creates_an_empty_auth_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let _store = FilesystemMetadataStore::new(dir.path().to_path_buf())
            .await
            .unwrap();

        assert!(auth_state_path(&dir).exists());
        assert!(persisted_auth_state(&dir).is_empty());
    }

    #[tokio::test]
    async fn legacy_timestamps_are_seeded_once_and_stripped_from_metadata() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _store = store_with_account(&dir).await;
        }
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(accounts_path(&dir)).unwrap()).unwrap();
        raw["acct"]["last_auth_timestamp"] = serde_json::json!(4242);
        std::fs::write(accounts_path(&dir), serde_json::to_string(&raw).unwrap()).unwrap();
        std::fs::remove_file(auth_state_path(&dir)).unwrap();

        let store = FilesystemMetadataStore::new(dir.path().to_path_buf())
            .await
            .unwrap();

        assert_eq!(
            persisted_auth_state(&dir).get(&auth_state_key("acct", SIGNER_A)),
            Some(&4242)
        );
        assert_eq!(
            persisted_auth_state(&dir).get(&auth_state_key("acct", SIGNER_B)),
            Some(&4242),
            "the legacy account-scoped floor must cover every authorized signer"
        );
        assert!(
            !store
                .update_last_auth_timestamp_cas("acct", SIGNER_A, 4242)
                .await
                .unwrap(),
            "seeded timestamp must be enforced"
        );
        assert!(
            store
                .update_last_auth_timestamp_cas("acct", SIGNER_A, 4243)
                .await
                .unwrap()
        );
        assert!(
            !std::fs::read_to_string(accounts_path(&dir))
                .unwrap()
                .contains("last_auth_timestamp"),
            "legacy values must be stripped so a lost auth-state file cannot re-seed stale state"
        );
    }

    #[tokio::test]
    async fn auth_state_file_loss_after_migration_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _store = store_with_account(&dir).await;
        }
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(accounts_path(&dir)).unwrap()).unwrap();
        raw["acct"]["last_auth_timestamp"] = serde_json::json!(4242);
        std::fs::write(accounts_path(&dir), serde_json::to_string(&raw).unwrap()).unwrap();
        std::fs::remove_file(auth_state_path(&dir)).unwrap();
        {
            let _seeded = FilesystemMetadataStore::new(dir.path().to_path_buf())
                .await
                .unwrap();
        }

        std::fs::remove_file(auth_state_path(&dir)).unwrap();
        let err = FilesystemMetadataStore::new(dir.path().to_path_buf())
            .await
            .err()
            .expect("losing replay state after migration must reject startup");
        assert!(
            err.contains("auth_state.json"),
            "error must name the missing file: {err}"
        );
    }

    #[tokio::test]
    async fn pre_split_store_with_null_timestamps_migrates_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _store = store_with_account(&dir).await;
        }
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(accounts_path(&dir)).unwrap()).unwrap();
        raw["acct"]["last_auth_timestamp"] = serde_json::Value::Null;
        std::fs::write(accounts_path(&dir), serde_json::to_string(&raw).unwrap()).unwrap();
        std::fs::remove_file(auth_state_path(&dir)).unwrap();

        let _store = FilesystemMetadataStore::new(dir.path().to_path_buf())
            .await
            .expect("a never-authenticated pre-split store is a legitimate first migration");

        assert!(persisted_auth_state(&dir).is_empty());
        assert!(
            !std::fs::read_to_string(accounts_path(&dir))
                .unwrap()
                .contains("last_auth_timestamp"),
            "null legacy keys must be stripped so later file loss still fails closed"
        );
    }

    #[tokio::test]
    async fn interrupted_migration_residue_is_merged_and_stripped_on_next_boot() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _store = store_with_account(&dir).await;
        }
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(accounts_path(&dir)).unwrap()).unwrap();
        raw["acct"]["last_auth_timestamp"] = serde_json::json!(5000);
        std::fs::write(accounts_path(&dir), serde_json::to_string(&raw).unwrap()).unwrap();
        std::fs::write(
            auth_state_path(&dir),
            serde_json::to_string(&HashMap::from([("acct".to_string(), 100_i64)])).unwrap(),
        )
        .unwrap();

        let store = FilesystemMetadataStore::new(dir.path().to_path_buf())
            .await
            .unwrap();

        assert_eq!(
            persisted_auth_state(&dir).get(&auth_state_key("acct", SIGNER_A)),
            Some(&5000),
            "the newer of legacy and auth-state values must win the merge"
        );
        assert!(
            !store
                .update_last_auth_timestamp_cas("acct", SIGNER_A, 5000)
                .await
                .unwrap()
        );
        assert!(
            !std::fs::read_to_string(accounts_path(&dir))
                .unwrap()
                .contains("last_auth_timestamp"),
            "legacy residue must be stripped whenever it is found"
        );
    }

    #[tokio::test]
    async fn stale_legacy_residue_never_regresses_auth_state() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _store = store_with_account(&dir).await;
        }
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(accounts_path(&dir)).unwrap()).unwrap();
        raw["acct"]["last_auth_timestamp"] = serde_json::json!(100);
        std::fs::write(accounts_path(&dir), serde_json::to_string(&raw).unwrap()).unwrap();
        std::fs::write(
            auth_state_path(&dir),
            serde_json::to_string(&HashMap::from([("acct".to_string(), 5000_i64)])).unwrap(),
        )
        .unwrap();

        let store = FilesystemMetadataStore::new(dir.path().to_path_buf())
            .await
            .unwrap();

        assert_eq!(
            persisted_auth_state(&dir).get(&auth_state_key("acct", SIGNER_A)),
            Some(&5000)
        );
        assert!(
            !store
                .update_last_auth_timestamp_cas("acct", SIGNER_A, 4999)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn persist_failure_rolls_back_in_memory_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_account(&dir).await;
        assert!(
            store
                .update_last_auth_timestamp_cas("acct", "0xaa", 50)
                .await
                .unwrap()
        );

        std::fs::remove_file(auth_state_path(&dir)).unwrap();
        std::fs::create_dir(auth_state_path(&dir)).unwrap();
        store
            .update_last_auth_timestamp_cas("acct", "0xaa", 100)
            .await
            .expect_err("persisting over a directory must fail");

        std::fs::remove_dir(auth_state_path(&dir)).unwrap();
        assert!(
            store
                .update_last_auth_timestamp_cas("acct", "0xaa", 100)
                .await
                .unwrap(),
            "a timestamp that was never durably recorded must not be treated as a replay"
        );
        assert_eq!(persisted_auth_state(&dir).get("acct:0xaa"), Some(&100));
        assert!(
            !store
                .update_last_auth_timestamp_cas("acct", "0xaa", 50)
                .await
                .unwrap(),
            "rollback must restore the prior value, not clear it"
        );
    }

    #[tokio::test]
    async fn operator_recreated_empty_auth_state_is_honored() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _store = store_with_account(&dir).await;
        }

        std::fs::remove_file(auth_state_path(&dir)).unwrap();
        std::fs::write(auth_state_path(&dir), "{}").unwrap();

        let store = FilesystemMetadataStore::new(dir.path().to_path_buf())
            .await
            .expect("an explicitly recreated auth-state file is an operator decision");
        assert!(
            store
                .update_last_auth_timestamp_cas("acct", "0xaa", 1)
                .await
                .unwrap()
        );
    }
}
