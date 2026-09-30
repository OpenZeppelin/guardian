//! Shared publication store for the `/dashboard/stats` aggregate (issue
//! #371, multi-replica design).
//!
//! One replica — the holder of the `dashboard_stats` lease — computes
//! the aggregate and publishes it here; every replica reads the same
//! published version, so requests routed to different replicas never
//! disagree on totals or `as_of`. The store also carries the refresh
//! control row: operator-triggered refresh requests, the in-progress
//! marker, and the cooldown clock, so automatic and operator-triggered
//! refreshes share one coordination mechanism.
//!
//! Publication is fenced: [`StatsStore::publish`] validates the
//! caller's lease inside the same transaction as the write and refuses
//! to overwrite a snapshot published under a newer fence token, so a
//! superseded holder that finishes a walk after losing leadership can
//! never replace a newer snapshot.
//!
//! The payload is opaque JSON to this module; the dashboard stats
//! module owns its schema.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::{GuardianError, Result};
use crate::storage::LeaseFence;

/// A published aggregate as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedStats {
    /// Monotonically increasing publication counter.
    pub version: i64,
    /// Time the publishing walk began (the response's `as_of`).
    pub as_of: DateTime<Utc>,
    pub published_at: DateTime<Utc>,
    pub payload: serde_json::Value,
}

/// The refresh control row. `now` is the store's own clock (database
/// time on Postgres) so callers compare against consistent timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsControl {
    pub now: DateTime<Utc>,
    pub last_published_at: Option<DateTime<Utc>>,
    /// Pending operator-triggered refresh, cleared by the publication
    /// of a walk that started after it.
    pub refresh_requested_at: Option<DateTime<Utc>>,
    pub refresh_requested_by: Option<String>,
    /// Set by the leader when a walk starts; cleared on publish or
    /// explicit failure. A marker older than the caller's staleness
    /// bound is treated as abandoned (leader crashed mid-walk).
    pub refresh_started_at: Option<DateTime<Utc>>,
    pub refresh_started_by: Option<String>,
    /// Clock for the operator-request cooldown.
    pub last_operator_request_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    Published {
        version: i64,
        published_at: DateTime<Utc>,
    },
    /// The caller's lease is no longer current, or a newer fence token
    /// has already published; nothing was written.
    StaleLease,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshRequestOutcome {
    Queued { requested_at: DateTime<Utc> },
    AlreadyQueued { requested_at: DateTime<Utc> },
    InProgress { started_at: DateTime<Utc> },
    Cooldown { retry_after: Duration },
}

/// Storage for the published aggregate and its refresh control state.
///
/// `now` parameters are honored by the in-memory store (single-process,
/// deterministic tests) and ignored by the Postgres store, which uses
/// the database clock so every replica agrees.
#[async_trait]
pub trait StatsStore: Send + Sync {
    /// Head version only — cheap enough for followers to poll.
    async fn current_version(&self) -> Result<Option<i64>>;

    async fn load_current(&self) -> Result<Option<PublishedStats>>;

    async fn read_control(&self, now: DateTime<Utc>) -> Result<StatsControl>;

    /// Record that the lease holder started a walk. `false` when the
    /// lease is no longer current (nothing recorded).
    async fn mark_refresh_started(&self, fence: &LeaseFence, now: DateTime<Utc>) -> Result<bool>;

    /// Clear the in-progress marker after a failed walk so operator
    /// requests are not reported as in progress forever.
    async fn clear_refresh_started(&self, fence: &LeaseFence) -> Result<()>;

    /// Fenced, atomic publication. Also clears the in-progress marker
    /// and any operator request the published walk already covered.
    async fn publish(
        &self,
        fence: &LeaseFence,
        as_of: DateTime<Utc>,
        now: DateTime<Utc>,
        payload: serde_json::Value,
    ) -> Result<PublishOutcome>;

    /// Queue an operator-triggered refresh, or report why it was not
    /// queued: a walk is already running (started within
    /// `stale_after`), a request is already pending, or the cooldown
    /// since the last operator request has not elapsed.
    async fn request_refresh(
        &self,
        requested_by: &str,
        now: DateTime<Utc>,
        cooldown: Duration,
        stale_after: Duration,
    ) -> Result<RefreshRequestOutcome>;
}

/// Single-process store for the filesystem backend and tests. Applies
/// the same fence-token ordering rule as the Postgres store so the
/// leader loop behaves identically on both backends.
#[derive(Default)]
pub struct InMemoryStatsStore {
    inner: Mutex<InMemoryState>,
}

#[derive(Default)]
struct InMemoryState {
    current: Option<(i64, PublishedStats)>, // (fence_token, snapshot)
    last_published_at: Option<DateTime<Utc>>,
    refresh_requested_at: Option<DateTime<Utc>>,
    refresh_requested_by: Option<String>,
    refresh_started_at: Option<DateTime<Utc>>,
    refresh_started_by: Option<String>,
    last_operator_request_at: Option<DateTime<Utc>>,
}

impl InMemoryStatsStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, InMemoryState> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

#[async_trait]
impl StatsStore for InMemoryStatsStore {
    async fn current_version(&self) -> Result<Option<i64>> {
        Ok(self.lock().current.as_ref().map(|(_, s)| s.version))
    }

    async fn load_current(&self) -> Result<Option<PublishedStats>> {
        Ok(self.lock().current.as_ref().map(|(_, s)| s.clone()))
    }

    async fn read_control(&self, now: DateTime<Utc>) -> Result<StatsControl> {
        let state = self.lock();
        Ok(StatsControl {
            now,
            last_published_at: state.last_published_at,
            refresh_requested_at: state.refresh_requested_at,
            refresh_requested_by: state.refresh_requested_by.clone(),
            refresh_started_at: state.refresh_started_at,
            refresh_started_by: state.refresh_started_by.clone(),
            last_operator_request_at: state.last_operator_request_at,
        })
    }

    async fn mark_refresh_started(&self, fence: &LeaseFence, now: DateTime<Utc>) -> Result<bool> {
        let mut state = self.lock();
        state.refresh_started_at = Some(now);
        state.refresh_started_by = Some(fence.holder_id.clone());
        Ok(true)
    }

    async fn clear_refresh_started(&self, fence: &LeaseFence) -> Result<()> {
        let mut state = self.lock();
        if state.refresh_started_by.as_deref() == Some(fence.holder_id.as_str()) {
            state.refresh_started_at = None;
            state.refresh_started_by = None;
        }
        Ok(())
    }

    async fn publish(
        &self,
        fence: &LeaseFence,
        as_of: DateTime<Utc>,
        now: DateTime<Utc>,
        payload: serde_json::Value,
    ) -> Result<PublishOutcome> {
        let mut state = self.lock();
        if let Some((published_token, _)) = &state.current
            && *published_token > fence.fence_token
        {
            return Ok(PublishOutcome::StaleLease);
        }
        let version = state.current.as_ref().map(|(_, s)| s.version).unwrap_or(0) + 1;
        let published = PublishedStats {
            version,
            as_of,
            published_at: now,
            payload,
        };
        state.current = Some((fence.fence_token, published));
        state.last_published_at = Some(now);
        if let (Some(requested), Some(started)) =
            (state.refresh_requested_at, state.refresh_started_at)
            && requested <= started
        {
            state.refresh_requested_at = None;
            state.refresh_requested_by = None;
        }
        state.refresh_started_at = None;
        state.refresh_started_by = None;
        Ok(PublishOutcome::Published {
            version,
            published_at: now,
        })
    }

    async fn request_refresh(
        &self,
        requested_by: &str,
        now: DateTime<Utc>,
        cooldown: Duration,
        stale_after: Duration,
    ) -> Result<RefreshRequestOutcome> {
        let mut state = self.lock();
        Ok(decide_request(
            &StatsControl {
                now,
                last_published_at: state.last_published_at,
                refresh_requested_at: state.refresh_requested_at,
                refresh_requested_by: state.refresh_requested_by.clone(),
                refresh_started_at: state.refresh_started_at,
                refresh_started_by: state.refresh_started_by.clone(),
                last_operator_request_at: state.last_operator_request_at,
            },
            cooldown,
            stale_after,
        )
        .unwrap_or_else(|| {
            state.refresh_requested_at = Some(now);
            state.refresh_requested_by = Some(requested_by.to_string());
            state.last_operator_request_at = Some(now);
            RefreshRequestOutcome::Queued { requested_at: now }
        }))
    }
}

/// Shared decision rule for [`StatsStore::request_refresh`]: `Some`
/// when the request must be refused/reported as-is, `None` when it
/// should be queued. Both backends apply it so their semantics match.
pub fn decide_request(
    control: &StatsControl,
    cooldown: Duration,
    stale_after: Duration,
) -> Option<RefreshRequestOutcome> {
    let stale_after = chrono::Duration::from_std(stale_after).unwrap_or(chrono::Duration::MAX);
    if let Some(started_at) = control.refresh_started_at
        && control.now - started_at < stale_after
    {
        return Some(RefreshRequestOutcome::InProgress { started_at });
    }
    if let Some(requested_at) = control.refresh_requested_at {
        return Some(RefreshRequestOutcome::AlreadyQueued { requested_at });
    }
    if let Some(last) = control.last_operator_request_at {
        let cooldown = chrono::Duration::from_std(cooldown).unwrap_or(chrono::Duration::MAX);
        let elapsed = control.now - last;
        if elapsed < cooldown {
            let remaining = (cooldown - elapsed)
                .to_std()
                .unwrap_or(Duration::from_secs(1));
            return Some(RefreshRequestOutcome::Cooldown {
                retry_after: remaining.max(Duration::from_secs(1)),
            });
        }
    }
    None
}

impl RefreshRequestOutcome {
    /// Map a refused request to the error the HTTP surface reports.
    pub fn cooldown_error(retry_after: Duration) -> GuardianError {
        GuardianError::RateLimitExceeded {
            retry_after_secs: retry_after.as_secs().clamp(1, u32::MAX as u64) as u32,
            scope: "dashboard_stats_refresh".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn fence(token: i64, holder: &str) -> LeaseFence {
        LeaseFence {
            lease_name: "dashboard_stats".into(),
            holder_id: holder.into(),
            fence_token: token,
        }
    }

    #[tokio::test]
    async fn publish_increments_version_and_rejects_older_fence_tokens() {
        let store = InMemoryStatsStore::new();
        assert_eq!(store.current_version().await.unwrap(), None);
        let now = ts("2026-09-15T10:00:00Z");
        let first = store
            .publish(&fence(1, "a"), now, now, serde_json::json!({"n": 1}))
            .await
            .unwrap();
        assert!(matches!(
            first,
            PublishOutcome::Published { version: 1, .. }
        ));
        // A newer holder (token 2) publishes version 2 ...
        let second = store
            .publish(&fence(2, "b"), now, now, serde_json::json!({"n": 2}))
            .await
            .unwrap();
        assert!(matches!(
            second,
            PublishOutcome::Published { version: 2, .. }
        ));
        // ... and the superseded holder (token 1) can no longer overwrite it.
        let stale = store
            .publish(&fence(1, "a"), now, now, serde_json::json!({"n": 3}))
            .await
            .unwrap();
        assert_eq!(stale, PublishOutcome::StaleLease);
        let current = store.load_current().await.unwrap().unwrap();
        assert_eq!(current.version, 2);
        assert_eq!(current.payload, serde_json::json!({"n": 2}));
    }

    #[tokio::test]
    async fn request_refresh_queues_then_reports_queued_in_progress_and_cooldown() {
        let store = InMemoryStatsStore::new();
        let cooldown = Duration::from_secs(60);
        let stale = Duration::from_secs(600);
        let t0 = ts("2026-09-15T10:00:00Z");

        let queued = store
            .request_refresh("op-1", t0, cooldown, stale)
            .await
            .unwrap();
        assert_eq!(queued, RefreshRequestOutcome::Queued { requested_at: t0 });
        let again = store
            .request_refresh("op-2", t0 + chrono::Duration::seconds(5), cooldown, stale)
            .await
            .unwrap();
        assert_eq!(
            again,
            RefreshRequestOutcome::AlreadyQueued { requested_at: t0 }
        );

        // The leader starts a walk: requests now report in-progress.
        let t1 = t0 + chrono::Duration::seconds(10);
        assert!(
            store
                .mark_refresh_started(&fence(1, "a"), t1)
                .await
                .unwrap()
        );
        let in_progress = store
            .request_refresh("op-3", t1, cooldown, stale)
            .await
            .unwrap();
        assert_eq!(
            in_progress,
            RefreshRequestOutcome::InProgress { started_at: t1 }
        );

        // Publishing a walk that started after the request consumes it,
        // and the cooldown then applies to the next operator request.
        let t2 = t0 + chrono::Duration::seconds(20);
        store
            .publish(&fence(1, "a"), t1, t2, serde_json::json!({}))
            .await
            .unwrap();
        let control = store.read_control(t2).await.unwrap();
        assert_eq!(control.refresh_requested_at, None);
        assert_eq!(control.refresh_started_at, None);
        assert_eq!(control.last_published_at, Some(t2));
        let cooling = store
            .request_refresh("op-4", t2, cooldown, stale)
            .await
            .unwrap();
        assert_eq!(
            cooling,
            RefreshRequestOutcome::Cooldown {
                retry_after: Duration::from_secs(40)
            }
        );
        let later = store
            .request_refresh("op-4", t0 + chrono::Duration::seconds(61), cooldown, stale)
            .await
            .unwrap();
        assert!(matches!(later, RefreshRequestOutcome::Queued { .. }));
    }

    #[tokio::test]
    async fn abandoned_in_progress_marker_does_not_block_requests_forever() {
        let store = InMemoryStatsStore::new();
        let t0 = ts("2026-09-15T10:00:00Z");
        store
            .mark_refresh_started(&fence(1, "a"), t0)
            .await
            .unwrap();
        let much_later = t0 + chrono::Duration::hours(1);
        let outcome = store
            .request_refresh(
                "op",
                much_later,
                Duration::from_secs(60),
                Duration::from_secs(600),
            )
            .await
            .unwrap();
        assert!(matches!(outcome, RefreshRequestOutcome::Queued { .. }));
    }

    #[tokio::test]
    async fn failed_walk_clears_only_its_own_marker() {
        let store = InMemoryStatsStore::new();
        let t0 = ts("2026-09-15T10:00:00Z");
        store
            .mark_refresh_started(&fence(2, "b"), t0)
            .await
            .unwrap();
        store.clear_refresh_started(&fence(1, "a")).await.unwrap();
        assert_eq!(
            store
                .read_control(t0)
                .await
                .unwrap()
                .refresh_started_by
                .as_deref(),
            Some("b")
        );
        store.clear_refresh_started(&fence(2, "b")).await.unwrap();
        assert_eq!(
            store.read_control(t0).await.unwrap().refresh_started_at,
            None
        );
    }
}
