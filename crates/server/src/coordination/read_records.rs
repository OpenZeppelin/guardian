//! Record of accepted session reads (issue #317).
//!
//! Under the `unique` read replay policy a signed session read is admitted at
//! most once, by its record key (SHA-256 of the session public key and the
//! signed envelope), instead of advancing a per-key timestamp floor. Two
//! independent checks refuse a delayed request: its timestamp is re-checked
//! at admission (freshness), and a purge first raises a watermark that
//! admission refuses timestamps at or below (fence), so a purged request is
//! never admitted again. Postgres keeps the record fleet-wide
//! (`coordination::postgres::read_records`); the file store here keeps it for
//! one process across restarts. The canonical nonce poll is the exception
//! ([`ReadRule::WindowOnly`]): it is accepted every time it is presented
//! inside a narrow window and records nothing.

use std::collections::{BTreeMap, HashSet};
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;

use super::session_store::{SessionKey, SessionSubject};
use crate::builder::clock::Clock;
use crate::metadata::auth::TimestampWindow;
use crate::state_object::StateHead;

/// Width of one record bucket (a Postgres partition, a file here).
pub const READ_RECORD_BUCKET_MS: i64 = 300_000;

pub type ReadRecordKey = [u8; 32];

/// What admitting one signed session read found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadRecordOutcome {
    /// First time this request was seen: admitted.
    Recorded,
    /// The same signed request was admitted before.
    Duplicate,
    /// Outside the timestamp window by the store's clock at admission.
    OutsideWindow,
    /// At or below the fence: its record may have been purged.
    Stale,
    /// Inside its window under [`ReadRule::WindowOnly`]: accepted, nothing
    /// recorded.
    Served,
}

impl ReadRecordOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Duplicate => "duplicate",
            Self::OutsideWindow => "outside_window",
            Self::Stale => "stale",
            Self::Served => "served",
        }
    }
}

/// How a session read is admitted once its session is found live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadRule {
    /// Admitted at most once: `key` is recorded, inside a window of
    /// `skew_ms` either side of the store's clock and above the fence.
    Record { key: ReadRecordKey, skew_ms: i64 },
    /// Accepted every time it is presented inside `window` by the store's
    /// clock. Nothing is recorded.
    WindowOnly(TimestampWindow),
}

impl ReadRule {
    pub fn window(self) -> TimestampWindow {
        match self {
            Self::Record { skew_ms, .. } => TimestampWindow::symmetric(skew_ms),
            Self::WindowOnly(window) => window,
        }
    }

    pub fn kind(self) -> &'static str {
        match self {
            Self::Record { .. } => "recorded",
            Self::WindowOnly(_) => "window_only",
        }
    }
}

/// What a session store says about a session key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Live(SessionSubject),
    Unknown,
    Revoked,
    Expired,
}

impl SessionStatus {
    fn live(self) -> Result<SessionSubject, InactiveSession> {
        match self {
            Self::Live(subject) => Ok(subject),
            Self::Unknown => Err(InactiveSession::Unknown),
            Self::Revoked => Err(InactiveSession::Revoked),
            Self::Expired => Err(InactiveSession::Expired),
        }
    }
}

/// The account and session lookups a store without its own access to them
/// uses for [`ReadRecordStore::admit_session_read`].
#[async_trait]
pub trait SessionReadLookups: Send + Sync {
    async fn account_exists(&self, account_id: &str) -> Result<bool, String>;
    async fn session(&self, key: &SessionKey) -> Result<SessionStatus, String>;
}

/// One session read to admit.
pub struct SessionReadRequest<'a> {
    pub account_id: &'a str,
    pub session_key: SessionKey,
    pub rule: ReadRule,
    pub timestamp_ms: i64,
    pub clock: &'a dyn Clock,
    pub head: HeadRead,
}

/// Whether a store that can read the canonical state head in the same step
/// as the admission should do so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadRead {
    Skip,
    Include,
}

/// The canonical state head read together with an accepted admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrefetchedHead {
    /// Not read with the admission; the caller reads it from storage.
    NotRead,
    /// The account has no stored state.
    Missing,
    Found(StateHead),
}

/// A session that cannot sign requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InactiveSession {
    Unknown,
    Revoked,
    Expired,
}

/// What admitting a session read found. Nothing is recorded unless the
/// account exists and the session is live.
/// `metadata_version` is the account's metadata version when the store reads
/// it in the same step, so the caller can use a cached copy at that version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionReadAdmission {
    AccountNotFound,
    Inactive(InactiveSession),
    /// A live session: the record was consulted.
    Live {
        metadata_version: Option<i64>,
        subject: SessionSubject,
        outcome: ReadRecordOutcome,
        head: PrefetchedHead,
    },
}

#[async_trait]
pub trait ReadRecordStore: Send + Sync {
    /// Look up the account and the session and, when both are there and the
    /// session is live, admit the read by its [`ReadRule`]: one database call
    /// on Postgres, `lookups` then [`Self::record`] or the window check
    /// elsewhere.
    async fn admit_session_read(
        &self,
        lookups: &dyn SessionReadLookups,
        request: SessionReadRequest<'_>,
    ) -> Result<SessionReadAdmission, String>;

    /// Admit the read `key` signed at `timestamp_ms`, checking its window
    /// against the time at admission: `clock` read under the store's lock,
    /// or the database clock on Postgres.
    async fn record(
        &self,
        key: ReadRecordKey,
        timestamp_ms: i64,
        skew_ms: i64,
        clock: &dyn Clock,
    ) -> Result<ReadRecordOutcome, String>;

    /// Make sure buckets exist from `now - retain_ms` to `now + ahead_ms`,
    /// raise the fence to just below the last bucket boundary at or below
    /// `now - retain_ms`, and drop what it covers. Returns the watermark.
    async fn maintain(&self, now_ms: i64, retain_ms: i64, ahead_ms: i64) -> Result<i64, String>;
}

/// [`ReadRecordStore::admit_session_read`] for stores that record locally.
async fn admit_with_lookups(
    store: &dyn ReadRecordStore,
    lookups: &dyn SessionReadLookups,
    request: SessionReadRequest<'_>,
) -> Result<SessionReadAdmission, String> {
    if !lookups.account_exists(request.account_id).await? {
        return Ok(SessionReadAdmission::AccountNotFound);
    }
    let subject = match lookups.session(&request.session_key).await?.live() {
        Ok(subject) => subject,
        Err(inactive) => return Ok(SessionReadAdmission::Inactive(inactive)),
    };
    let outcome = match request.rule {
        ReadRule::Record { key, skew_ms } => {
            let outcome = store
                .record(key, request.timestamp_ms, skew_ms, request.clock)
                .await?;
            if outcome == ReadRecordOutcome::Recorded
                && let Err(inactive) = lookups.session(&request.session_key).await?.live()
            {
                return Ok(SessionReadAdmission::Inactive(inactive));
            }
            outcome
        }
        ReadRule::WindowOnly(window) => {
            if window.contains(request.timestamp_ms, request.clock.now().timestamp_millis()) {
                ReadRecordOutcome::Served
            } else {
                ReadRecordOutcome::OutsideWindow
            }
        }
    };
    Ok(SessionReadAdmission::Live {
        metadata_version: None,
        subject,
        outcome,
        head: PrefetchedHead::NotRead,
    })
}

/// The watermark a purge at `now_ms` keeping `retain_ms` sets.
pub fn fence_watermark(now_ms: i64, retain_ms: i64) -> i64 {
    (now_ms - retain_ms).div_euclid(READ_RECORD_BUCKET_MS) * READ_RECORD_BUCKET_MS - 1
}

fn bucket_of(timestamp_ms: i64) -> i64 {
    timestamp_ms.div_euclid(READ_RECORD_BUCKET_MS) * READ_RECORD_BUCKET_MS
}

struct LocalState {
    watermark: i64,
    seen: BTreeMap<i64, HashSet<ReadRecordKey>>,
}

impl LocalState {
    fn admit(
        &mut self,
        key: ReadRecordKey,
        timestamp_ms: i64,
        skew_ms: i64,
        now_ms: i64,
    ) -> ReadRecordOutcome {
        if now_ms.abs_diff(timestamp_ms) > skew_ms as u64 {
            return ReadRecordOutcome::OutsideWindow;
        }
        if timestamp_ms <= self.watermark {
            return ReadRecordOutcome::Stale;
        }
        if self
            .seen
            .entry(bucket_of(timestamp_ms))
            .or_default()
            .insert(key)
        {
            ReadRecordOutcome::Recorded
        } else {
            ReadRecordOutcome::Duplicate
        }
    }

    /// Raise the watermark and drop the buckets wholly at or below it.
    /// Returns the dropped bucket starts.
    fn advance(&mut self, watermark: i64) -> Vec<i64> {
        self.watermark = self.watermark.max(watermark);
        let covered: Vec<i64> = self
            .seen
            .keys()
            .copied()
            .filter(|start| start + READ_RECORD_BUCKET_MS <= self.watermark + 1)
            .collect();
        for start in &covered {
            self.seen.remove(start);
        }
        covered
    }
}

/// A process-local record, lost on restart. For tests and in-memory setups.
pub struct InMemoryReadRecords {
    state: Mutex<LocalState>,
}

impl InMemoryReadRecords {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(LocalState {
                watermark: i64::MIN,
                seen: BTreeMap::new(),
            }),
        }
    }
}

impl Default for InMemoryReadRecords {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ReadRecordStore for InMemoryReadRecords {
    async fn admit_session_read(
        &self,
        lookups: &dyn SessionReadLookups,
        request: SessionReadRequest<'_>,
    ) -> Result<SessionReadAdmission, String> {
        admit_with_lookups(self, lookups, request).await
    }

    async fn record(
        &self,
        key: ReadRecordKey,
        timestamp_ms: i64,
        skew_ms: i64,
        clock: &dyn Clock,
    ) -> Result<ReadRecordOutcome, String> {
        let mut state = self.state.lock().map_err(|_| "read record lock poisoned")?;
        let now_ms = clock.now().timestamp_millis();
        Ok(state.admit(key, timestamp_ms, skew_ms, now_ms))
    }

    async fn maintain(&self, now_ms: i64, retain_ms: i64, _ahead_ms: i64) -> Result<i64, String> {
        let mut state = self.state.lock().map_err(|_| "read record lock poisoned")?;
        state.advance(fence_watermark(now_ms, retain_ms));
        Ok(state.watermark)
    }
}

/// A record kept in one append-only file per bucket under `dir`, flushed to
/// disk before a read is admitted, with the watermark in `dir/fence`. It
/// survives restarts: a session's store forgets the session on restart and
/// the same grant can be registered again, so a record that did not survive
/// would let a request signed ahead of server time be admitted twice.
pub struct FileReadRecords {
    dir: PathBuf,
    state: Mutex<LocalState>,
}

impl FileReadRecords {
    /// Opens the record, failing closed: a missing fence while records exist,
    /// or a corrupt record line, refuses to start rather than forget a read.
    /// Only a torn last line (a write that never returned, so its read was
    /// never admitted) is cut off.
    pub fn open(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
        let mut buckets = Vec::new();
        for entry in
            std::fs::read_dir(dir).map_err(|e| format!("Failed to list {}: {e}", dir.display()))?
        {
            let path = entry.map_err(|e| e.to_string())?.path();
            if let Some(start) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|_| path.extension().is_some_and(|ext| ext == "log"))
                .and_then(|stem| stem.parse::<i64>().ok())
            {
                buckets.push((start, path));
            }
        }
        let watermark = match std::fs::read_to_string(dir.join(FENCE_FILE)) {
            Ok(text) => text
                .trim()
                .parse::<i64>()
                .map_err(|e| format!("Invalid read record fence: {e}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && buckets.is_empty() => {
                write_fence(dir, i64::MIN)?;
                i64::MIN
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "Read record fence is missing in {} while records exist; refusing to start",
                    dir.display()
                ));
            }
            Err(error) => return Err(format!("Failed to read read record fence: {error}")),
        };
        let mut state = LocalState {
            watermark,
            seen: BTreeMap::new(),
        };
        for (start, path) in buckets {
            if start + READ_RECORD_BUCKET_MS <= watermark + 1 {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            load_bucket(&path, state.seen.entry(start).or_default())?;
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            state: Mutex::new(state),
        })
    }

    fn bucket_path(&self, start: i64) -> PathBuf {
        self.dir.join(format!("{start}.log"))
    }

    fn append(&self, start: i64, key: &ReadRecordKey) -> Result<(), String> {
        let path = self.bucket_path(start);
        let created = !path.exists();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("Failed to open {}: {e}", path.display()))?;
        writeln!(file, "{}", hex::encode(key))
            .and_then(|()| file.sync_data())
            .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
        if created {
            sync_dir(&self.dir)?;
        }
        Ok(())
    }
}

const FENCE_FILE: &str = "fence";

fn parse_record(line: &str) -> Option<ReadRecordKey> {
    let bytes = hex::decode(line.trim()).ok()?;
    ReadRecordKey::try_from(bytes.as_slice()).ok()
}

fn load_bucket(path: &Path, bucket: &mut HashSet<ReadRecordKey>) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let complete = text.rfind('\n').map_or(0, |end| end + 1);
    for line in text[..complete].lines() {
        let key = parse_record(line).ok_or_else(|| {
            format!(
                "Corrupt read record in {}: {line:?}; refusing to start",
                path.display()
            )
        })?;
        bucket.insert(key);
    }
    let torn = &text[complete..];
    if torn.is_empty() {
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| format!("Failed to open {}: {e}", path.display()))?;
    match parse_record(torn) {
        Some(key) => {
            bucket.insert(key);
            file.seek(std::io::SeekFrom::End(0))
                .and_then(|_| file.write_all(b"\n"))
        }
        None => file.set_len(complete as u64),
    }
    .and_then(|()| file.sync_data())
    .map_err(|e| format!("Failed to repair {}: {e}", path.display()))
}

fn sync_dir(dir: &Path) -> Result<(), String> {
    std::fs::File::open(dir)
        .and_then(|handle| handle.sync_all())
        .map_err(|e| format!("Failed to sync {}: {e}", dir.display()))
}

/// Replaces the fence durably (temp file, sync, rename, directory sync), so
/// buckets are only unlinked once the raised fence is on disk.
fn write_fence(dir: &Path, watermark: i64) -> Result<(), String> {
    let temp = dir.join(format!("{FENCE_FILE}.tmp"));
    let mut file = std::fs::File::create(&temp)
        .map_err(|e| format!("Failed to create {}: {e}", temp.display()))?;
    file.write_all(watermark.to_string().as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("Failed to write {}: {e}", temp.display()))?;
    std::fs::rename(&temp, dir.join(FENCE_FILE))
        .map_err(|e| format!("Failed to replace the read record fence: {e}"))?;
    sync_dir(dir)
}

#[async_trait]
impl ReadRecordStore for FileReadRecords {
    async fn admit_session_read(
        &self,
        lookups: &dyn SessionReadLookups,
        request: SessionReadRequest<'_>,
    ) -> Result<SessionReadAdmission, String> {
        admit_with_lookups(self, lookups, request).await
    }

    async fn record(
        &self,
        key: ReadRecordKey,
        timestamp_ms: i64,
        skew_ms: i64,
        clock: &dyn Clock,
    ) -> Result<ReadRecordOutcome, String> {
        let mut state = self.state.lock().map_err(|_| "read record lock poisoned")?;
        let now_ms = clock.now().timestamp_millis();
        let outcome = state.admit(key, timestamp_ms, skew_ms, now_ms);
        if outcome == ReadRecordOutcome::Recorded
            && let Err(error) = self.append(bucket_of(timestamp_ms), &key)
        {
            if let Some(bucket) = state.seen.get_mut(&bucket_of(timestamp_ms)) {
                bucket.remove(&key);
            }
            return Err(error);
        }
        Ok(outcome)
    }

    async fn maintain(&self, now_ms: i64, retain_ms: i64, _ahead_ms: i64) -> Result<i64, String> {
        let mut state = self.state.lock().map_err(|_| "read record lock poisoned")?;
        let watermark = fence_watermark(now_ms, retain_ms).max(state.watermark);
        if watermark > state.watermark {
            write_fence(&self.dir, watermark)?;
        }
        for start in state.advance(watermark) {
            let _ = std::fs::remove_file(self.bucket_path(start));
        }
        Ok(state.watermark)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::clock::test::MockClock;

    const SKEW: i64 = 300_000;
    const NOW: i64 = 1_800_000_000_000;

    fn key(byte: u8) -> ReadRecordKey {
        [byte; 32]
    }

    fn at(ms: i64) -> MockClock {
        MockClock::new(chrono::DateTime::from_timestamp_millis(ms).unwrap())
    }

    async fn admits_once_and_refuses_stale(store: &dyn ReadRecordStore) {
        assert_eq!(
            store.record(key(1), NOW, SKEW, &at(NOW)).await.unwrap(),
            ReadRecordOutcome::Recorded
        );
        assert_eq!(
            store.record(key(1), NOW, SKEW, &at(NOW)).await.unwrap(),
            ReadRecordOutcome::Duplicate
        );
        assert_eq!(
            store.record(key(2), NOW, SKEW, &at(NOW)).await.unwrap(),
            ReadRecordOutcome::Recorded,
            "another request with the same timestamp is admitted"
        );
        assert_eq!(
            store
                .record(key(3), NOW - SKEW - 1, SKEW, &at(NOW))
                .await
                .unwrap(),
            ReadRecordOutcome::OutsideWindow,
            "delayed past its window, no purge needed"
        );
    }

    /// Lookups that take longer than the timestamp window.
    struct SlowLookups<'a> {
        clock: &'a MockClock,
    }

    #[async_trait]
    impl SessionReadLookups for SlowLookups<'_> {
        async fn account_exists(&self, _account_id: &str) -> Result<bool, String> {
            Ok(true)
        }

        async fn session(&self, _key: &SessionKey) -> Result<SessionStatus, String> {
            self.clock.advance_secs(SKEW / 1_000 + 1);
            Ok(SessionStatus::Live(SessionSubject::Miden {
                signer_commitment: "0x1".to_string(),
                origin: String::new(),
                guardian_commitment: "0x2".to_string(),
                network: "devnet".to_string(),
            }))
        }
    }

    async fn checks_the_window_at_admission(store: &dyn ReadRecordStore) {
        let clock = at(NOW);
        let admission = store
            .admit_session_read(
                &SlowLookups { clock: &clock },
                SessionReadRequest {
                    account_id: "0xaccount",
                    session_key: key(7),
                    rule: ReadRule::Record {
                        key: key(8),
                        skew_ms: SKEW,
                    },
                    timestamp_ms: NOW,
                    clock: &clock,
                    head: HeadRead::Include,
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            admission,
            SessionReadAdmission::Live {
                outcome: ReadRecordOutcome::OutsideWindow,
                head: PrefetchedHead::NotRead,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_read_delayed_past_its_window_before_admission_is_refused() {
        checks_the_window_at_admission(&InMemoryReadRecords::new()).await;
        let dir = tempfile::tempdir().unwrap();
        checks_the_window_at_admission(&FileReadRecords::open(dir.path()).unwrap()).await;
    }

    struct LiveLookups;

    #[async_trait]
    impl SessionReadLookups for LiveLookups {
        async fn account_exists(&self, _account_id: &str) -> Result<bool, String> {
            Ok(true)
        }

        async fn session(&self, _key: &SessionKey) -> Result<SessionStatus, String> {
            Ok(SessionStatus::Live(SessionSubject::Miden {
                signer_commitment: "0x1".to_string(),
                origin: String::new(),
                guardian_commitment: "0x2".to_string(),
                network: "devnet".to_string(),
            }))
        }
    }

    async fn window_only(store: &dyn ReadRecordStore, timestamp_ms: i64) -> ReadRecordOutcome {
        let clock = at(NOW);
        match store
            .admit_session_read(
                &LiveLookups,
                SessionReadRequest {
                    account_id: "0xaccount",
                    session_key: key(7),
                    rule: ReadRule::WindowOnly(TimestampWindow {
                        past_ms: 60_000,
                        future_ms: 30_000,
                    }),
                    timestamp_ms,
                    clock: &clock,
                    head: HeadRead::Include,
                },
            )
            .await
            .unwrap()
        {
            SessionReadAdmission::Live { outcome, .. } => outcome,
            other => panic!("expected a live session, got {other:?}"),
        }
    }

    async fn serves_window_only_reads_repeatedly_inside_their_window(store: &dyn ReadRecordStore) {
        for _ in 0..3 {
            assert_eq!(window_only(store, NOW).await, ReadRecordOutcome::Served);
        }
        assert_eq!(
            window_only(store, NOW - 60_000).await,
            ReadRecordOutcome::Served
        );
        assert_eq!(
            window_only(store, NOW + 30_000).await,
            ReadRecordOutcome::Served
        );
        assert_eq!(
            window_only(store, NOW - 60_001).await,
            ReadRecordOutcome::OutsideWindow
        );
        assert_eq!(
            window_only(store, NOW + 30_001).await,
            ReadRecordOutcome::OutsideWindow
        );
    }

    #[tokio::test]
    async fn window_only_reads_are_served_repeatedly_inside_their_window() {
        serves_window_only_reads_repeatedly_inside_their_window(&InMemoryReadRecords::new()).await;
        let dir = tempfile::tempdir().unwrap();
        serves_window_only_reads_repeatedly_inside_their_window(
            &FileReadRecords::open(dir.path()).unwrap(),
        )
        .await;
    }

    struct ExpiresAfterFirstLookup(std::sync::atomic::AtomicBool);

    #[async_trait]
    impl SessionReadLookups for ExpiresAfterFirstLookup {
        async fn account_exists(&self, _account_id: &str) -> Result<bool, String> {
            Ok(true)
        }

        async fn session(&self, key: &SessionKey) -> Result<SessionStatus, String> {
            if self.0.swap(true, std::sync::atomic::Ordering::SeqCst) {
                Ok(SessionStatus::Expired)
            } else {
                LiveLookups.session(key).await
            }
        }
    }

    async fn refuses_a_read_whose_session_expired_while_it_was_recorded(
        store: &dyn ReadRecordStore,
    ) {
        let clock = at(NOW);
        let admission = store
            .admit_session_read(
                &ExpiresAfterFirstLookup(Default::default()),
                SessionReadRequest {
                    account_id: "0xaccount",
                    session_key: key(7),
                    rule: ReadRule::Record {
                        key: key(9),
                        skew_ms: SKEW,
                    },
                    timestamp_ms: NOW,
                    clock: &clock,
                    head: HeadRead::Include,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            admission,
            SessionReadAdmission::Inactive(InactiveSession::Expired)
        );
    }

    #[tokio::test]
    async fn a_session_expiring_while_its_read_is_recorded_is_refused() {
        refuses_a_read_whose_session_expired_while_it_was_recorded(&InMemoryReadRecords::new())
            .await;
        let dir = tempfile::tempdir().unwrap();
        refuses_a_read_whose_session_expired_while_it_was_recorded(
            &FileReadRecords::open(dir.path()).unwrap(),
        )
        .await;
    }

    #[tokio::test]
    async fn in_memory_admits_each_request_once_and_refuses_stale_ones() {
        admits_once_and_refuses_stale(&InMemoryReadRecords::new()).await;
    }

    #[tokio::test]
    async fn file_store_admits_each_request_once_and_refuses_stale_ones() {
        let dir = tempfile::tempdir().unwrap();
        admits_once_and_refuses_stale(&FileReadRecords::open(dir.path()).unwrap()).await;
    }

    #[tokio::test]
    async fn a_purged_request_is_never_admitted_again_even_with_the_window_check_relaxed() {
        let store = InMemoryReadRecords::new();
        let old = NOW - 2 * READ_RECORD_BUCKET_MS;
        assert_eq!(
            store
                .record(key(1), old, i64::MAX / 4, &at(NOW))
                .await
                .unwrap(),
            ReadRecordOutcome::Recorded
        );
        let watermark = store.maintain(NOW, 0, 0).await.unwrap();
        assert!(old <= watermark);
        assert_eq!(
            store
                .record(key(1), old, i64::MAX / 4, &at(NOW))
                .await
                .unwrap(),
            ReadRecordOutcome::Stale
        );
    }

    #[tokio::test]
    async fn the_file_store_remembers_a_future_dated_read_across_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let ahead = NOW + 120_000;
        {
            let store = FileReadRecords::open(dir.path()).unwrap();
            assert_eq!(
                store.record(key(9), ahead, SKEW, &at(NOW)).await.unwrap(),
                ReadRecordOutcome::Recorded
            );
        }
        let restarted = FileReadRecords::open(dir.path()).unwrap();
        assert_eq!(
            restarted
                .record(key(9), ahead, SKEW, &at(NOW + 10_000))
                .await
                .unwrap(),
            ReadRecordOutcome::Duplicate
        );
    }

    #[tokio::test]
    async fn the_file_store_keeps_its_fence_and_drops_covered_buckets_across_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let old = NOW - 2 * READ_RECORD_BUCKET_MS;
        {
            let store = FileReadRecords::open(dir.path()).unwrap();
            store
                .record(key(1), old, i64::MAX / 4, &at(NOW))
                .await
                .unwrap();
            store.maintain(NOW, 0, 0).await.unwrap();
        }
        assert!(!dir.path().join(format!("{}.log", bucket_of(old))).exists());
        let restarted = FileReadRecords::open(dir.path()).unwrap();
        assert_eq!(
            restarted
                .record(key(1), old, i64::MAX / 4, &at(NOW))
                .await
                .unwrap(),
            ReadRecordOutcome::Stale
        );
    }

    #[tokio::test]
    async fn the_file_store_refuses_to_start_without_its_fence_while_records_exist() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = FileReadRecords::open(dir.path()).unwrap();
            store.record(key(1), NOW, SKEW, &at(NOW)).await.unwrap();
        }
        std::fs::remove_file(dir.path().join(FENCE_FILE)).unwrap();
        assert!(FileReadRecords::open(dir.path()).is_err());
    }

    #[tokio::test]
    async fn the_file_store_refuses_to_start_on_a_corrupt_record() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = FileReadRecords::open(dir.path()).unwrap();
            store.record(key(1), NOW, SKEW, &at(NOW)).await.unwrap();
        }
        let bucket = dir.path().join(format!("{}.log", bucket_of(NOW)));
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&bucket)
            .unwrap();
        writeln!(file, "not-a-record").unwrap();
        assert!(FileReadRecords::open(dir.path()).is_err());
    }

    #[tokio::test]
    async fn the_file_store_cuts_a_torn_last_record_and_keeps_appending() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = FileReadRecords::open(dir.path()).unwrap();
            store.record(key(1), NOW, SKEW, &at(NOW)).await.unwrap();
        }
        let bucket = dir.path().join(format!("{}.log", bucket_of(NOW)));
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&bucket)
            .unwrap();
        write!(file, "{}", &hex::encode(key(2))[..10]).unwrap();
        drop(file);
        {
            let restarted = FileReadRecords::open(dir.path()).unwrap();
            assert_eq!(
                restarted.record(key(1), NOW, SKEW, &at(NOW)).await.unwrap(),
                ReadRecordOutcome::Duplicate
            );
            assert_eq!(
                restarted.record(key(2), NOW, SKEW, &at(NOW)).await.unwrap(),
                ReadRecordOutcome::Recorded
            );
        }
        let again = FileReadRecords::open(dir.path()).unwrap();
        assert_eq!(
            again.record(key(2), NOW, SKEW, &at(NOW)).await.unwrap(),
            ReadRecordOutcome::Duplicate
        );
    }
}
