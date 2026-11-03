//! Shared state of block watch (issue #585): the cursor (the last block
//! fully processed) and the observations the feed hands to the release
//! detectors.
//!
//! Block watch runs on the `release_sweep` lease holder. Everything a
//! successor needs to carry on lives here, so a failover resumes from the
//! cursor and visits the observations the previous holder recorded but did
//! not get to. On Postgres both are shared tables and the cursor is written
//! only under the holder's lease. The filesystem backend is one process:
//! the cursor survives a restart in a file, the observations are in memory
//! and are lost with the process (a restart requests one rotation of the
//! release sweep instead).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::{GuardianError, Result};
use crate::network::NetworkType;
use crate::storage::LeaseFence;

/// A guarded account that moved on chain, in block `block_num`, to a
/// commitment this server did not hold when the block was processed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub account_id: String,
    pub block_num: u32,
    pub on_chain_commitment: String,
    /// Visits that ended without checking the account (a chain read
    /// failed, or the visit itself failed).
    pub attempts: u32,
}

/// The last block fully processed, and the chain it was counted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub last_block: u32,
    /// The genesis commitment of the chain `last_block` belongs to, when
    /// the node reported one: a different one means the chain was reset.
    pub genesis: Option<String>,
}

/// The walks of the release sweep rotation block watch asked for, as
/// sequence numbers: requests raise `requested_seq`, a completed walk
/// raises `completed_seq` to the sequence it started under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkRequests {
    pub requested_seq: i64,
    pub completed_seq: i64,
}

impl WalkRequests {
    /// A walk is owed: a request came after the last completed walk
    /// started.
    pub fn owed(&self) -> bool {
        self.requested_seq > self.completed_seq
    }
}

/// Outcome of a fenced write that is not a cursor move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FencedWrite {
    Written,
    /// The caller's lease is no longer current; nothing was written.
    StaleLease,
}

/// One observation to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewObservation {
    pub account_id: String,
    pub block_num: u32,
    pub on_chain_commitment: String,
}

/// Outcome of [`BlockWatchStore::set_cursor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorWrite {
    /// The cursor now stands at the requested block.
    Advanced,
    /// The cursor already stood at or past the requested block; nothing
    /// was written. A cursor never moves backwards.
    NotForward,
    /// The caller's lease is no longer current; nothing was written. The
    /// caller has lost the release sweep lease and must stop.
    StaleLease,
}

#[async_trait]
pub trait BlockWatchStore: Send + Sync {
    /// The last block fully processed for `network`, if any.
    async fn cursor(&self, network: NetworkType) -> Result<Option<Cursor>>;

    /// Move the cursor forward to `block` of the chain whose genesis is
    /// `genesis` (`None` keeps the recorded one). Fenced by `fence` on
    /// backends that share state across replicas (Postgres): a write under
    /// a lease that is no longer current is refused as
    /// [`CursorWrite::StaleLease`].
    async fn set_cursor(
        &self,
        network: NetworkType,
        block: u32,
        genesis: Option<&str>,
        fence: &LeaseFence,
    ) -> Result<CursorWrite>;

    /// Set the cursor to `block` whatever it stood at: the chain was reset,
    /// and heights of the old chain mean nothing on the new one. Fenced
    /// like [`Self::set_cursor`].
    async fn reset_cursor(
        &self,
        network: NetworkType,
        block: u32,
        genesis: Option<&str>,
        fence: &LeaseFence,
    ) -> Result<CursorWrite>;

    /// Record observations, due at once. Idempotent per `(account_id,
    /// block_num)`: replaying a block never duplicates one, and an
    /// observation already recorded keeps its attempts and due time.
    async fn record_observations(
        &self,
        observations: &[NewObservation],
        now: DateTime<Utc>,
    ) -> Result<()>;

    /// The due observation of the lowest block (then account id), if any.
    async fn next_due_observation(&self, now: DateTime<Utc>) -> Result<Option<Observation>>;

    /// When the earliest observation not yet due becomes due, if any. The
    /// visitor sleeps until then instead of polling.
    async fn next_observation_due_at(&self) -> Result<Option<DateTime<Utc>>>;

    async fn delete_observation(&self, account_id: &str, block_num: u32) -> Result<()>;

    /// Count one more unsuccessful visit and make the observation due again
    /// at `next_attempt_at`.
    async fn defer_observation(
        &self,
        account_id: &str,
        block_num: u32,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<()>;

    /// Make the observation due again at `next_attempt_at` without
    /// counting an attempt: the visit waited (nothing failed).
    async fn postpone_observation(
        &self,
        account_id: &str,
        block_num: u32,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<()>;

    /// Observations awaiting a visit, due or not.
    async fn count_pending_observations(&self) -> Result<u64>;

    /// The walks requested and completed for `network`, if any was ever
    /// requested.
    async fn walk_requests(&self, network: NetworkType) -> Result<Option<WalkRequests>>;

    /// Record a request for one walk of the release sweep rotation (block
    /// watch skipped blocks), at `now`. Fenced like
    /// [`Self::set_cursor`].
    async fn request_walk(
        &self,
        network: NetworkType,
        now: DateTime<Utc>,
        fence: &LeaseFence,
    ) -> Result<FencedWrite>;

    /// A walk that started when `requested_seq` stood at `started_under`
    /// completed: every request up to it is covered. Never moves
    /// `completed_seq` back. Fenced like [`Self::set_cursor`].
    async fn complete_walk(
        &self,
        network: NetworkType,
        started_under: i64,
        fence: &LeaseFence,
    ) -> Result<FencedWrite>;
}

#[derive(Debug, Clone)]
struct StoredObservation {
    on_chain_commitment: String,
    attempts: u32,
    next_attempt_at: DateTime<Utc>,
}

/// On-disk form of the filesystem backend's cursor file: one entry per
/// network, keyed by the network's name.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct CursorFile {
    #[serde(flatten)]
    cursors: BTreeMap<String, CursorEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CursorEntry {
    last_block: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    genesis: Option<String>,
    updated_at: DateTime<Utc>,
}

/// The single-process store: observations in memory, the cursor in memory
/// and, when built with [`Self::persistent`], in `<dir>/cursor.json` too.
/// No fencing: a single process holds the only lease there is.
#[derive(Default)]
pub struct InMemoryBlockWatchStore {
    cursor_file: Option<PathBuf>,
    cursors: Mutex<HashMap<String, Cursor>>,
    /// Held across a cursor write's check, file write and commit, so two
    /// writes cannot interleave and move the cursor back.
    cursor_write: tokio::sync::Mutex<()>,
    /// Keyed `(block_num, account_id)`: iteration order is visit order.
    observations: Mutex<BTreeMap<(u32, String), StoredObservation>>,
    /// Walk requests, per network. In memory only: a restart of the single
    /// process requests a walk anyway (its observations are gone too).
    walks: Mutex<HashMap<String, WalkRequests>>,
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

impl InMemoryBlockWatchStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A store whose cursor survives restarts in `<dir>/cursor.json`
    /// (created on the first write). An existing file is read now; one that
    /// cannot be read or parsed fails startup rather than silently starting
    /// over.
    pub fn persistent(dir: PathBuf) -> std::result::Result<Self, String> {
        let path = dir.join("cursor.json");
        let cursors = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<CursorFile>(&bytes)
                .map_err(|e| format!("block watch cursor file {} is invalid: {e}", path.display()))?
                .cursors
                .into_iter()
                .map(|(network, entry)| {
                    (
                        network,
                        Cursor {
                            last_block: entry.last_block,
                            genesis: entry.genesis,
                        },
                    )
                })
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                return Err(format!(
                    "failed to read block watch cursor file {}: {e}",
                    path.display()
                ));
            }
        };
        Ok(Self {
            cursor_file: Some(path),
            cursors: Mutex::new(cursors),
            cursor_write: tokio::sync::Mutex::new(()),
            observations: Mutex::default(),
            walks: Mutex::default(),
        })
    }

    /// Write the cursor file atomically: a temporary file in the same
    /// directory, then a rename over the old one.
    async fn persist(&self, snapshot: HashMap<String, Cursor>) -> Result<()> {
        let Some(path) = &self.cursor_file else {
            return Ok(());
        };
        let now = Utc::now();
        let file = CursorFile {
            cursors: snapshot
                .into_iter()
                .map(|(network, cursor)| {
                    (
                        network,
                        CursorEntry {
                            last_block: cursor.last_block,
                            genesis: cursor.genesis,
                            updated_at: now,
                        },
                    )
                })
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&file)
            .map_err(|e| GuardianError::StorageError(format!("block watch cursor encode: {e}")))?;
        let dir = path.parent().map(PathBuf::from).unwrap_or_default();
        let temp = dir.join(format!("cursor.json.{:016x}.tmp", rand::random::<u64>()));
        let write = async {
            tokio::fs::create_dir_all(&dir).await?;
            tokio::fs::write(&temp, &bytes).await?;
            tokio::fs::rename(&temp, path).await
        };
        if let Err(e) = write.await {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(GuardianError::StorageError(format!(
                "block watch cursor write {}: {e}",
                path.display()
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl BlockWatchStore for InMemoryBlockWatchStore {
    async fn cursor(&self, network: NetworkType) -> Result<Option<Cursor>> {
        Ok(locked(&self.cursors).get(&network.to_string()).cloned())
    }

    async fn set_cursor(
        &self,
        network: NetworkType,
        block: u32,
        genesis: Option<&str>,
        _fence: &LeaseFence,
    ) -> Result<CursorWrite> {
        let _writing = self.cursor_write.lock().await;
        let key = network.to_string();
        let (cursor, snapshot) = {
            let cursors = locked(&self.cursors);
            let current = cursors.get(&key);
            if current.is_some_and(|current| current.last_block >= block) {
                return Ok(CursorWrite::NotForward);
            }
            let genesis = genesis
                .map(str::to_string)
                .or_else(|| current.and_then(|current| current.genesis.clone()));
            let cursor = Cursor {
                last_block: block,
                genesis,
            };
            let mut snapshot = cursors.clone();
            snapshot.insert(key.clone(), cursor.clone());
            (cursor, snapshot)
        };
        // The file first: a failed write leaves the cursor where it was, so
        // the retried block writes it again instead of finding it already
        // moved (`NotForward`) and never persisting it.
        self.persist(snapshot).await?;
        locked(&self.cursors).insert(key, cursor);
        Ok(CursorWrite::Advanced)
    }

    async fn reset_cursor(
        &self,
        network: NetworkType,
        block: u32,
        genesis: Option<&str>,
        _fence: &LeaseFence,
    ) -> Result<CursorWrite> {
        let _writing = self.cursor_write.lock().await;
        let key = network.to_string();
        let cursor = Cursor {
            last_block: block,
            genesis: genesis.map(str::to_string),
        };
        let mut snapshot = locked(&self.cursors).clone();
        snapshot.insert(key.clone(), cursor.clone());
        self.persist(snapshot).await?;
        locked(&self.cursors).insert(key, cursor);
        Ok(CursorWrite::Advanced)
    }

    async fn record_observations(
        &self,
        observations: &[NewObservation],
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut stored = locked(&self.observations);
        for observation in observations {
            stored
                .entry((observation.block_num, observation.account_id.clone()))
                .or_insert_with(|| StoredObservation {
                    on_chain_commitment: observation.on_chain_commitment.clone(),
                    attempts: 0,
                    next_attempt_at: now,
                });
        }
        Ok(())
    }

    async fn next_due_observation(&self, now: DateTime<Utc>) -> Result<Option<Observation>> {
        Ok(locked(&self.observations)
            .iter()
            .find(|(_, stored)| stored.next_attempt_at <= now)
            .map(|((block_num, account_id), stored)| Observation {
                account_id: account_id.clone(),
                block_num: *block_num,
                on_chain_commitment: stored.on_chain_commitment.clone(),
                attempts: stored.attempts,
            }))
    }

    async fn next_observation_due_at(&self) -> Result<Option<DateTime<Utc>>> {
        Ok(locked(&self.observations)
            .values()
            .map(|stored| stored.next_attempt_at)
            .min())
    }

    async fn delete_observation(&self, account_id: &str, block_num: u32) -> Result<()> {
        locked(&self.observations).remove(&(block_num, account_id.to_string()));
        Ok(())
    }

    async fn defer_observation(
        &self,
        account_id: &str,
        block_num: u32,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<()> {
        if let Some(stored) =
            locked(&self.observations).get_mut(&(block_num, account_id.to_string()))
        {
            stored.attempts = stored.attempts.saturating_add(1);
            stored.next_attempt_at = next_attempt_at;
        }
        Ok(())
    }

    async fn postpone_observation(
        &self,
        account_id: &str,
        block_num: u32,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<()> {
        if let Some(stored) =
            locked(&self.observations).get_mut(&(block_num, account_id.to_string()))
        {
            stored.next_attempt_at = next_attempt_at;
        }
        Ok(())
    }

    async fn count_pending_observations(&self) -> Result<u64> {
        Ok(locked(&self.observations).len() as u64)
    }

    async fn walk_requests(&self, network: NetworkType) -> Result<Option<WalkRequests>> {
        Ok(locked(&self.walks).get(&network.to_string()).copied())
    }

    async fn request_walk(
        &self,
        network: NetworkType,
        _now: DateTime<Utc>,
        _fence: &LeaseFence,
    ) -> Result<FencedWrite> {
        let mut walks = locked(&self.walks);
        let walk = walks.entry(network.to_string()).or_insert(WalkRequests {
            requested_seq: 0,
            completed_seq: 0,
        });
        walk.requested_seq = walk.requested_seq.saturating_add(1);
        Ok(FencedWrite::Written)
    }

    async fn complete_walk(
        &self,
        network: NetworkType,
        started_under: i64,
        _fence: &LeaseFence,
    ) -> Result<FencedWrite> {
        if let Some(walk) = locked(&self.walks).get_mut(&network.to_string()) {
            walk.completed_seq = walk.completed_seq.max(started_under);
        }
        Ok(FencedWrite::Written)
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use chrono::Duration;

    fn fence() -> LeaseFence {
        LeaseFence {
            lease_name: crate::coordination::RELEASE_SWEEP_LEASE.to_string(),
            holder_id: "single-process".to_string(),
            fence_token: 0,
        }
    }

    fn observed(account_id: &str, block_num: u32, commitment: &str) -> NewObservation {
        NewObservation {
            account_id: account_id.to_string(),
            block_num,
            on_chain_commitment: commitment.to_string(),
        }
    }

    #[tokio::test]
    async fn the_cursor_only_moves_forward_and_per_network() {
        let store = InMemoryBlockWatchStore::new();
        assert_eq!(store.cursor(NetworkType::MidenDevnet).await.unwrap(), None);
        assert_eq!(
            store
                .set_cursor(NetworkType::MidenDevnet, 10, Some("0xgenesis"), &fence())
                .await
                .unwrap(),
            CursorWrite::Advanced
        );
        for stale in [10, 9] {
            assert_eq!(
                store
                    .set_cursor(NetworkType::MidenDevnet, stale, None, &fence())
                    .await
                    .unwrap(),
                CursorWrite::NotForward
            );
        }
        // A write that does not name the chain keeps the recorded one.
        store
            .set_cursor(NetworkType::MidenDevnet, 11, None, &fence())
            .await
            .unwrap();
        assert_eq!(
            store.cursor(NetworkType::MidenDevnet).await.unwrap(),
            Some(Cursor {
                last_block: 11,
                genesis: Some("0xgenesis".into())
            })
        );
        assert_eq!(store.cursor(NetworkType::MidenTestnet).await.unwrap(), None);

        // A reset chain moves the cursor back, onto the new chain.
        assert_eq!(
            store
                .reset_cursor(NetworkType::MidenDevnet, 3, Some("0xnew"), &fence())
                .await
                .unwrap(),
            CursorWrite::Advanced
        );
        assert_eq!(
            store.cursor(NetworkType::MidenDevnet).await.unwrap(),
            Some(Cursor {
                last_block: 3,
                genesis: Some("0xnew".into())
            })
        );
    }

    #[tokio::test]
    async fn a_persistent_cursor_survives_a_restart() {
        let dir =
            std::env::temp_dir().join(format!("guardian_block_watch_{}", uuid::Uuid::new_v4()));
        let store = InMemoryBlockWatchStore::persistent(dir.clone()).unwrap();
        assert_eq!(store.cursor(NetworkType::MidenLocal).await.unwrap(), None);
        store
            .set_cursor(NetworkType::MidenLocal, 41, Some("0xgenesis"), &fence())
            .await
            .unwrap();
        store
            .set_cursor(NetworkType::MidenLocal, 42, None, &fence())
            .await
            .unwrap();
        store
            .record_observations(&[observed("0xa", 42, "0xc")], Utc::now())
            .await
            .unwrap();

        let restarted = InMemoryBlockWatchStore::persistent(dir.clone()).unwrap();
        assert_eq!(
            restarted.cursor(NetworkType::MidenLocal).await.unwrap(),
            Some(Cursor {
                last_block: 42,
                genesis: Some("0xgenesis".into())
            })
        );
        assert_eq!(
            restarted.count_pending_observations().await.unwrap(),
            0,
            "observations are in memory only on the filesystem backend"
        );
        // No temporary files are left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(leftovers, vec!["cursor.json".to_string()]);
    }

    #[tokio::test]
    async fn a_failed_cursor_write_leaves_the_cursor_unmoved_so_the_retry_writes_it() {
        // The cursor directory is replaced by a file: the write fails. The
        // cursor must not have moved, or the retry of the block would find
        // it `NotForward` and the file would never record the block (PR
        // #586 review).
        let dir =
            std::env::temp_dir().join(format!("guardian_block_watch_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = InMemoryBlockWatchStore::persistent(dir.clone()).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::write(&dir, b"not a directory").unwrap();
        assert!(
            store
                .set_cursor(NetworkType::MidenLocal, 41, Some("0xgenesis"), &fence())
                .await
                .is_err()
        );
        assert_eq!(store.cursor(NetworkType::MidenLocal).await.unwrap(), None);

        std::fs::remove_file(&dir).unwrap();
        assert_eq!(
            store
                .set_cursor(NetworkType::MidenLocal, 41, Some("0xgenesis"), &fence())
                .await
                .unwrap(),
            CursorWrite::Advanced,
            "the retry writes the block"
        );
        let restarted = InMemoryBlockWatchStore::persistent(dir.clone()).unwrap();
        assert_eq!(
            restarted
                .cursor(NetworkType::MidenLocal)
                .await
                .unwrap()
                .map(|cursor| cursor.last_block),
            Some(41)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_cursor_file_without_a_genesis_reads_as_an_unknown_chain() {
        let dir =
            std::env::temp_dir().join(format!("guardian_block_watch_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("cursor.json"),
            br#"{"MidenDevnet":{"last_block":7,"updated_at":"2026-11-02T00:00:00Z"}}"#,
        )
        .unwrap();
        let store = InMemoryBlockWatchStore::persistent(dir).unwrap();
        assert_eq!(
            store.cursor(NetworkType::MidenDevnet).await.unwrap(),
            Some(Cursor {
                last_block: 7,
                genesis: None
            })
        );
    }

    #[test]
    fn a_corrupt_cursor_file_fails_startup() {
        let dir =
            std::env::temp_dir().join(format!("guardian_block_watch_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cursor.json"), b"{not json").unwrap();
        let error = InMemoryBlockWatchStore::persistent(dir)
            .err()
            .expect("corrupt file");
        assert!(error.contains("invalid"), "{error}");
    }

    #[tokio::test]
    async fn observations_are_idempotent_and_visited_in_block_order() {
        let store = InMemoryBlockWatchStore::new();
        let now = Utc::now();
        store
            .record_observations(
                &[
                    observed("0xb", 12, "0xb12"),
                    observed("0xa", 12, "0xa12"),
                    observed("0xc", 11, "0xc11"),
                ],
                now,
            )
            .await
            .unwrap();
        // A replay of block 12 changes nothing, not even a later commitment.
        store
            .record_observations(&[observed("0xa", 12, "0xother")], now)
            .await
            .unwrap();
        assert_eq!(store.count_pending_observations().await.unwrap(), 3);

        let first = store.next_due_observation(now).await.unwrap().unwrap();
        assert_eq!((first.account_id.as_str(), first.block_num), ("0xc", 11));
        store.delete_observation("0xc", 11).await.unwrap();
        let second = store.next_due_observation(now).await.unwrap().unwrap();
        assert_eq!(
            (
                second.account_id.as_str(),
                second.block_num,
                second.on_chain_commitment.as_str()
            ),
            ("0xa", 12, "0xa12")
        );
    }

    #[tokio::test]
    async fn a_deferred_observation_waits_and_counts_its_attempts() {
        let store = InMemoryBlockWatchStore::new();
        let now = Utc::now();
        store
            .record_observations(
                &[observed("0xa", 5, "0xa5"), observed("0xb", 6, "0xb6")],
                now,
            )
            .await
            .unwrap();
        let later = now + Duration::seconds(60);
        store.defer_observation("0xa", 5, later).await.unwrap();

        let due = store.next_due_observation(now).await.unwrap().unwrap();
        assert_eq!(
            due.account_id, "0xb",
            "the deferred one is skipped until due"
        );
        store.delete_observation("0xb", 6).await.unwrap();
        assert_eq!(store.next_due_observation(now).await.unwrap(), None);
        assert_eq!(store.next_observation_due_at().await.unwrap(), Some(later));

        let again = store.next_due_observation(later).await.unwrap().unwrap();
        assert_eq!((again.account_id.as_str(), again.attempts), ("0xa", 1));
        // Deferring or deleting an observation that is gone is a no-op.
        store.defer_observation("0xgone", 1, later).await.unwrap();
        store.delete_observation("0xgone", 1).await.unwrap();
        assert_eq!(store.count_pending_observations().await.unwrap(), 1);
    }
}
