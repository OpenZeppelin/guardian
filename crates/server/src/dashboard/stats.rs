//! Shared, lease-held inventory and Miden vault aggregates for
//! `GET /dashboard/stats` and `GET /dashboard/info` (issue #371).
//!
//! The cross-operator dashboard used to reconstruct "assets under
//! guard" client-side: a full walk of `GET /dashboard/accounts` plus
//! one `GET /dashboard/accounts/{id}/snapshot` per recently-updated
//! account — roughly 1,100 requests per refresh on a 2,400-account
//! Guardian, which the code-default HTTP rate limits cut off. Only the
//! server can aggregate its stored states within a bounded budget, so
//! one replica does it and every replica serves the same published
//! result.
//!
//! # Roles
//!
//! - **Leader** (holder of the [`DASHBOARD_STATS_LEASE`] lease, one per
//!   fleet): every [`STATS_TICK`] it re-acquires the lease, reads the
//!   refresh control row, and runs a walk when the configured interval
//!   has elapsed since the last publication or an operator requested
//!   one. The walk is published through
//!   [`crate::coordination::StatsStore::publish`], which validates the
//!   lease inside the same transaction and refuses to overwrite a
//!   snapshot published under a newer fence token, so a superseded
//!   holder can never replace a newer result.
//! - **Every replica** (leader included): every [`STATS_TICK`] it polls
//!   the store's head version and, when it changed, loads the published
//!   payload into its in-memory [`DashboardStatsCache`]. Requests only
//!   read that cache — no storage reads, no vault decoding (FR-5 / FR-6).
//!
//! On the filesystem backend the store is in-process and the lease is
//! always held, so the same code runs single-process with identical
//! coverage and failure semantics.
//!
//! # Walk (per refresh)
//!
//! 1. `metadata.list()` for the authoritative id set, then
//!    `metadata.list_all()` (one snapshot on filesystem; indexed pages
//!    of [`STATS_PAGE_SIZE`] on Postgres). Any listed id the paged walk
//!    missed (a row bumped past the cursor mid-walk) is read individually.
//! 2. Miden states are batch-pulled in chunks of [`STATS_PAGE_SIZE`]. A
//!    failed batch is retried one account at a time so a **single
//!    corrupt or undecryptable row** becomes explicit `state_undecodable`
//!    coverage while a **systemic** storage or key-provider failure
//!    aborts the walk and leaves the previous snapshot published.
//! 3. Vaults are decoded on the blocking pool, once per commitment: an
//!    account whose state commitment matches the previous snapshot
//!    reuses its decoded vault (every state blob is still read and
//!    decrypted each walk; only the `Account` deserialization is
//!    skipped). Only successful decodes are reused, so a repaired blob
//!    is picked up on the next walk.
//! 4. Inventory aggregates (`delta_status_counts`,
//!    `in_flight_proposal_count`, `latest_activity`) are read through
//!    the storage aggregate methods; each is marked degraded on failure
//!    (including the filesystem inventory threshold) rather than
//!    failing the walk.
//!
//! # Freshness
//!
//! The configured interval is the cadence at which the leader *starts*
//! walks, not a bound on snapshot age: a slow or failing walk keeps the
//! previous snapshot published, and `as_of` is the only truthful age
//! signal. Until the first publication after a fresh deployment the
//! endpoint answers `503 data_unavailable`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use guardian_shared::FromJson;
use metrics::{counter, gauge, histogram};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::builder::clock::Clock;
use crate::coordination::{
    AlwaysLeader, DASHBOARD_STATS_LEASE, LeaderElector, Lease, PublishOutcome, PublishedStats,
};
use crate::metadata::{AccountMetadata, MetadataStore};
use crate::metrics::names::{
    DASHBOARD_STATS_REFRESH_DURATION_SECONDS, DASHBOARD_STATS_REFRESH_FAILURES_TOTAL,
    DASHBOARD_STATS_REFRESH_TIMESTAMP_SECONDS,
};
use crate::services::normalized_authorized_signer_count;
use crate::services::{AGG_DELTA_STATUS_COUNTS, AGG_IN_FLIGHT_PROPOSAL_COUNT, AGG_LATEST_ACTIVITY};
use crate::state::AppState;
use crate::storage::{
    LeaseFence, StorageBackend, StorageType, is_record_corruption, is_storage_not_found,
};

/// Accounts read per metadata page and per batched state pull during
/// a refresh. Bounds the working set of one storage round trip
/// independently of inventory size (FR-6).
pub const STATS_PAGE_SIZE: u32 = 200;

/// Cadence of the leader's due-check and of every replica's head-version
/// poll. Followers observe a new publication within this bound.
pub const STATS_TICK: Duration = Duration::from_secs(5);

/// Lease TTL for the refresher; renewed every [`STATS_LEASE_RENEW`]
/// while a walk runs. Failover after a crash happens within one TTL.
pub const STATS_LEASE_TTL: Duration = Duration::from_secs(60);
pub const STATS_LEASE_RENEW: Duration = Duration::from_secs(15);

/// Minimum spacing between accepted operator-triggered refresh requests.
pub const STATS_REFRESH_COOLDOWN: Duration = Duration::from_secs(60);

/// An in-progress marker older than this is treated as abandoned (the
/// leader crashed mid-walk) rather than reported as "in progress" to
/// operator requests. Two lease TTLs: by then a crashed holder has been
/// replaced and the new holder's walk has set its own marker.
pub const STATS_REFRESH_STALE_AFTER: Duration = Duration::from_secs(2 * 60);

/// After a failed walk the leader waits this long before walking again
/// (a full inventory walk every tick would otherwise hammer storage
/// while the fault persists).
pub const STATS_FAILURE_BACKOFF: Duration = Duration::from_secs(60);

/// Records per precomputed prefix block for `updated_since` queries.
const PREFIX_BLOCK: usize = 256;

/// Schema version of the persisted payload; bump on incompatible changes
/// so an older replica refuses (rather than misreads) a newer payload.
pub const STATS_PAYLOAD_SCHEMA: u32 = 1;

/// Mutually exclusive lifecycle bucket (FR-3): `released` wins over
/// `paused`, which wins over `active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountLifecycle {
    Active,
    Paused,
    Released,
}

impl AccountLifecycle {
    pub fn of(metadata: &AccountMetadata) -> Self {
        if metadata.released_at.is_some() {
            Self::Released
        } else if metadata.paused_at.is_some() {
            Self::Paused
        } else {
            Self::Active
        }
    }
}

/// Stable reason an eligible account's vault was not aggregated.
/// Serialized via [`SkipReason::as_str`]; new reasons must add a new
/// label, never repurpose an existing one.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Metadata exists but no state row could be read.
    StateUnavailable,
    /// The state row exists but its blob is corrupt: it does not
    /// decrypt, or does not deserialize as a Miden `Account`.
    StateUndecodable,
}

impl SkipReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StateUnavailable => "state_unavailable",
            Self::StateUndecodable => "state_undecodable",
        }
    }
}

/// Per-account vault totals, keyed by faucet id hex. Per-vault fungible
/// amounts fit `u64` (Miden caps them below 2^63); the cross-account
/// sums are widened to `u128` so they can never overflow.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultSummary {
    pub fungible: BTreeMap<String, u64>,
    pub non_fungible: BTreeMap<String, u64>,
}

/// What the refresh learned about one account's vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VaultOutcome {
    /// Decoded from the state row at `AccountStatsRecord::state_commitment`.
    Decoded {
        vault: VaultSummary,
    },
    Skipped {
        reason: SkipReason,
    },
    /// EVM account: no Miden vault exists, never eligible.
    NotApplicable,
}

/// One account's contribution to the aggregate, as persisted in the
/// published payload and held in memory on every replica.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStatsRecord {
    pub account_id: String,
    /// Parsed metadata `updated_at` (the value `DashboardAccountSummary`
    /// exposes). `None` when the stored string is not RFC3339; such a
    /// record still counts toward totals but never matches a time
    /// window or `updated_since` filter.
    pub updated_at: Option<DateTime<Utc>>,
    pub auth_method: String,
    pub authorized_signer_count: usize,
    pub lifecycle: AccountLifecycle,
    /// Commitment of the state row the vault outcome was derived from;
    /// `None` when no row could be read (or for EVM accounts). Lets the
    /// next refresh reuse a decoded vault when the state is unchanged.
    pub state_commitment: Option<String>,
    pub vault: VaultOutcome,
}

/// Delta lifecycle counts as persisted in the payload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotDeltaCounts {
    pub candidate: u64,
    pub canonical: u64,
    pub retained: u64,
    pub discarded: u64,
}

impl From<crate::storage::DeltaStatusCounts> for SnapshotDeltaCounts {
    fn from(c: crate::storage::DeltaStatusCounts) -> Self {
        Self {
            candidate: c.candidate,
            canonical: c.canonical,
            retained: c.retained,
            discarded: c.discarded,
        }
    }
}

/// Cross-account inventory aggregates captured in the same walk so
/// `/dashboard/info` reports one consistent snapshot time. A `None`
/// whose name appears in `degraded` was not computed; a `None`
/// `latest_activity` without that marker means no activity yet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryAggregates {
    pub delta_status_counts: Option<SnapshotDeltaCounts>,
    pub in_flight_proposal_count: Option<u64>,
    pub latest_activity: Option<DateTime<Utc>>,
    /// Stable aggregate names (`AGG_*`) the walk declined to compute.
    pub degraded: Vec<String>,
}

impl InventoryAggregates {
    /// Fully computed aggregates (tests and single-process helpers).
    pub fn available(
        delta_status_counts: SnapshotDeltaCounts,
        in_flight_proposal_count: u64,
        latest_activity: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            delta_status_counts: Some(delta_status_counts),
            in_flight_proposal_count: Some(in_flight_proposal_count),
            latest_activity,
            degraded: Vec::new(),
        }
    }
}

/// The persisted publication payload. Opaque to the coordination store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsPayload {
    pub schema: u32,
    pub records: Vec<AccountStatsRecord>,
    pub inventory: InventoryAggregates,
}

/// Unfiltered account counts (FR-3), computed once per publication.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountCounts {
    pub total: u64,
    pub active: u64,
    pub paused: u64,
    pub released: u64,
    pub by_auth_method: BTreeMap<String, u64>,
    /// `(auth_method, authorized_signer_count) -> accounts`.
    pub by_auth_method_and_signer_count: BTreeMap<(String, usize), u64>,
    pub updated_within_7d: u64,
    pub updated_within_30d: u64,
}

/// Asset totals plus the coverage that qualifies them (FR-1 / FR-4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetAggregate {
    pub eligible: u64,
    pub covered: u64,
    pub skipped: BTreeMap<SkipReason, u64>,
    pub fungible: BTreeMap<String, u128>,
    pub non_fungible: BTreeMap<String, u64>,
}

impl AssetAggregate {
    pub fn skipped_total(&self) -> u64 {
        self.skipped.values().sum()
    }

    /// `true` only when every eligible account was covered.
    pub fn complete(&self) -> bool {
        self.skipped_total() == 0 && self.covered == self.eligible
    }

    fn add(&mut self, record: &AccountStatsRecord) {
        match &record.vault {
            VaultOutcome::NotApplicable => {}
            VaultOutcome::Skipped { reason } => {
                self.eligible += 1;
                *self.skipped.entry(*reason).or_insert(0) += 1;
            }
            VaultOutcome::Decoded { vault } => {
                self.eligible += 1;
                self.covered += 1;
                for (faucet, amount) in &vault.fungible {
                    *self.fungible.entry(faucet.clone()).or_insert(0) += u128::from(*amount);
                }
                for (faucet, count) in &vault.non_fungible {
                    *self.non_fungible.entry(faucet.clone()).or_insert(0) += count;
                }
            }
        }
    }
}

/// A published snapshot as held in memory by one replica. `records` is
/// sorted by `updated_at` descending (unparseable timestamps last) so an
/// `updated_since` cutoff is a prefix; `prefix[k]` is the aggregate of
/// the first `k * PREFIX_BLOCK` records so a filtered query folds at
/// most one block instead of the whole inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub version: i64,
    pub as_of: DateTime<Utc>,
    pub published_at: DateTime<Utc>,
    records: Vec<AccountStatsRecord>,
    pub accounts: AccountCounts,
    pub inventory: InventoryAggregates,
    all_assets: AssetAggregate,
    prefix: Vec<AssetAggregate>,
}

impl StatsSnapshot {
    pub fn new(
        version: i64,
        as_of: DateTime<Utc>,
        published_at: DateTime<Utc>,
        mut records: Vec<AccountStatsRecord>,
        inventory: InventoryAggregates,
    ) -> Self {
        records.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.account_id.cmp(&b.account_id))
        });
        let accounts = Self::count_accounts(as_of, &records);
        let mut prefix = Vec::with_capacity(records.len() / PREFIX_BLOCK + 1);
        let mut running = AssetAggregate::default();
        prefix.push(running.clone());
        for (i, record) in records.iter().enumerate() {
            running.add(record);
            if (i + 1) % PREFIX_BLOCK == 0 {
                prefix.push(running.clone());
            }
        }
        Self {
            version,
            as_of,
            published_at,
            records,
            accounts,
            inventory,
            all_assets: running,
            prefix,
        }
    }

    /// Decode a payload published by any replica.
    pub fn from_published(published: &PublishedStats) -> Result<Self, String> {
        let payload: StatsPayload = serde_json::from_value(published.payload.clone())
            .map_err(|e| format!("published stats payload does not decode: {e}"))?;
        if payload.schema != STATS_PAYLOAD_SCHEMA {
            return Err(format!(
                "published stats payload schema {} is not the supported {}",
                payload.schema, STATS_PAYLOAD_SCHEMA
            ));
        }
        Ok(Self::new(
            published.version,
            published.as_of,
            published.published_at,
            payload.records,
            payload.inventory,
        ))
    }

    #[cfg(test)]
    pub fn records(&self) -> &[AccountStatsRecord] {
        &self.records
    }

    /// Asset totals over accounts whose metadata `updated_at >= since`
    /// (FR-2); `None` aggregates every account from the precomputed
    /// total. A cutoff folds the prefix block plus at most
    /// `PREFIX_BLOCK` records, independent of inventory size.
    pub fn assets_since(&self, since: Option<DateTime<Utc>>) -> AssetAggregate {
        let Some(since) = since else {
            return self.all_assets.clone();
        };
        let end = self
            .records
            .partition_point(|r| r.updated_at.is_some_and(|ts| ts >= since));
        let block = end / PREFIX_BLOCK;
        let mut aggregate = self.prefix[block].clone();
        for record in &self.records[block * PREFIX_BLOCK..end] {
            aggregate.add(record);
        }
        aggregate
    }

    fn count_accounts(as_of: DateTime<Utc>, records: &[AccountStatsRecord]) -> AccountCounts {
        let seven_days_ago = as_of - chrono::Duration::days(7);
        let thirty_days_ago = as_of - chrono::Duration::days(30);
        let mut counts = AccountCounts::default();
        for record in records {
            counts.total += 1;
            match record.lifecycle {
                AccountLifecycle::Active => counts.active += 1,
                AccountLifecycle::Paused => counts.paused += 1,
                AccountLifecycle::Released => counts.released += 1,
            }
            *counts
                .by_auth_method
                .entry(record.auth_method.clone())
                .or_insert(0) += 1;
            *counts
                .by_auth_method_and_signer_count
                .entry((record.auth_method.clone(), record.authorized_signer_count))
                .or_insert(0) += 1;
            // Windows are anchored to `as_of` on both sides: a row bumped
            // past `as_of` while the walk ran is not "within the last 7
            // days" of the time this snapshot represents.
            if let Some(updated_at) = record.updated_at.filter(|ts| *ts <= as_of) {
                if updated_at >= seven_days_ago {
                    counts.updated_within_7d += 1;
                }
                if updated_at >= thirty_days_ago {
                    counts.updated_within_30d += 1;
                }
            }
        }
        counts
    }
}

/// Per-replica holder of the latest snapshot loaded from the shared
/// store. Reads are a single `Arc` clone under a short read lock.
#[derive(Default)]
pub struct DashboardStatsCache {
    current: RwLock<Option<Arc<StatsSnapshot>>>,
}

impl std::fmt::Debug for DashboardStatsCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let current = self.current();
        f.debug_struct("DashboardStatsCache")
            .field("version", &current.as_ref().map(|s| s.version))
            .field("as_of", &current.as_ref().map(|s| s.as_of))
            .finish()
    }
}

impl DashboardStatsCache {
    /// Latest loaded snapshot, or `None` until one has been published
    /// and synced.
    pub fn current(&self) -> Option<Arc<StatsSnapshot>> {
        self.current
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub fn publish(&self, snapshot: Arc<StatsSnapshot>) {
        *self
            .current
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(snapshot);
    }

    /// Drop the loaded copy when the store confirms there is no
    /// publication to serve (row deleted, control table reset, restore
    /// from a pre-feature backup, plaintext row refused).
    pub fn clear(&self) {
        *self
            .current
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = None;
    }
}

/// Spawn the two background loops: the leader refresh loop and the
/// follower sync loop (every replica runs both; only the lease holder
/// ever walks the inventory).
pub fn start_stats_refresher(state: AppState, leader: Arc<dyn LeaderElector>) {
    tokio::spawn(run_sync_loop(state.clone()));
    tokio::spawn(run_leader_loop(state, leader));
}

async fn run_sync_loop(state: AppState) {
    // Warn once per distinct failure (e.g. a newer payload schema during
    // a rolling deploy) rather than on every tick.
    let mut last_error: Option<String> = None;
    loop {
        match sync_from_store(&state).await {
            Ok(_) => last_error = None,
            Err(error) => {
                if last_error.as_deref() != Some(error.as_str()) {
                    tracing::warn!(
                        target: "dashboard.stats",
                        %error,
                        "dashboard stats: could not sync the published snapshot; serving the last loaded one"
                    );
                    last_error = Some(error);
                } else {
                    tracing::debug!(target: "dashboard.stats", %error, "dashboard stats: sync still failing");
                }
            }
        }
        tokio::time::sleep(STATS_TICK).await;
    }
}

/// Load the store's current publication into this replica's cache when
/// its version differs from the one already loaded.
pub async fn sync_from_store(state: &AppState) -> Result<Option<Arc<StatsSnapshot>>, String> {
    let store = state.dashboard.stats_store();
    let cache = state.dashboard.stats();
    let head = store.current_version().await.map_err(|e| e.to_string())?;
    let loaded = cache.current();
    if head == loaded.as_ref().map(|s| s.version) {
        return Ok(loaded);
    }
    let Some(published) = store.load_current().await.map_err(|e| e.to_string())? else {
        // The store confirms there is nothing to serve (or refused what
        // it holds): cache and store must not diverge.
        if loaded.is_some() {
            tracing::warn!(
                target: "dashboard.stats",
                "dashboard stats: store has no publication; dropping the loaded snapshot"
            );
            cache.clear();
        }
        return Ok(None);
    };
    let snapshot = Arc::new(StatsSnapshot::from_published(&published)?);
    cache.publish(snapshot.clone());
    tracing::debug!(
        target: "dashboard.stats",
        version = snapshot.version,
        as_of = %snapshot.as_of,
        "dashboard stats: loaded published snapshot"
    );
    Ok(Some(snapshot))
}

async fn run_leader_loop(state: AppState, leader: Arc<dyn LeaderElector>) {
    let interval = state.dashboard.stats_refresh_interval();
    let mut backoff_until: Option<Instant> = None;
    loop {
        // While backing off after a failed walk this replica does not
        // touch the lease at all: acquiring would renew it and keep every
        // healthy replica locked out for as long as the local fault lasts.
        if backoff_until.is_none_or(|until| Instant::now() >= until) {
            backoff_until = match leader_tick(&state, &leader, interval).await {
                TickOutcome::WalkFailed => Some(Instant::now() + STATS_FAILURE_BACKOFF),
                _ => None,
            };
        }
        tokio::time::sleep(STATS_TICK).await;
    }
}

/// One leader-loop tick.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TickOutcome {
    /// Another replica holds the lease (or acquisition failed).
    NotLeader,
    /// Leader, but nothing was due.
    Idle,
    /// Leader and a walk was published.
    Published,
    /// Leader and the walk failed; the lease was released so a healthy
    /// replica can take over immediately instead of after the TTL.
    WalkFailed,
}

pub(crate) async fn leader_tick(
    state: &AppState,
    leader: &Arc<dyn LeaderElector>,
    interval: Duration,
) -> TickOutcome {
    let lease = match leader.try_acquire(STATS_LEASE_TTL).await {
        Ok(Some(lease)) => lease,
        Ok(None) => return TickOutcome::NotLeader,
        Err(error) => {
            tracing::warn!(
                target: "dashboard.stats",
                %error,
                "dashboard stats: could not acquire the refresher lease"
            );
            return TickOutcome::NotLeader;
        }
    };
    match refresh_if_due(state, leader, lease.clone(), interval).await {
        Ok(Some(_)) => TickOutcome::Published,
        Ok(None) => TickOutcome::Idle,
        Err(error) => {
            tracing::warn!(
                target: "dashboard.stats",
                %error,
                backoff_secs = STATS_FAILURE_BACKOFF.as_secs(),
                "dashboard stats refresh failed; previous snapshot left published, lease released"
            );
            if let Err(release_error) = leader.release(lease).await {
                tracing::warn!(
                    target: "dashboard.stats",
                    error = %release_error,
                    "dashboard stats: could not release the refresher lease after a failed walk"
                );
            }
            TickOutcome::WalkFailed
        }
    }
}

/// Leader step: walk when the interval elapsed since the last
/// publication (or nothing was ever published) or an operator requested
/// a refresh. Returns the published snapshot when a walk ran.
async fn refresh_if_due(
    state: &AppState,
    leader: &Arc<dyn LeaderElector>,
    lease: Lease,
    interval: Duration,
) -> Result<Option<Arc<StatsSnapshot>>, String> {
    let control = state
        .dashboard
        .stats_store()
        .read_control(state.clock.now())
        .await
        .map_err(|e| e.to_string())?;
    let interval = chrono::Duration::from_std(interval).unwrap_or(chrono::Duration::MAX);
    let due_by_interval = control
        .last_published_at
        .is_none_or(|last| control.now - last >= interval);
    let requested = control.refresh_requested_at.is_some();
    // The lease already guarantees a single walker fleet-wide and
    // publication is fenced, so a leftover in-progress marker from a
    // crashed holder never gates the new holder: `mark_refresh_started`
    // simply overwrites it.
    if !(due_by_interval || requested) {
        return Ok(None);
    }
    refresh_dashboard_stats_as(state, leader, lease)
        .await
        .map(Some)
}

/// Run one walk as the holder of `lease` and publish it through the
/// shared store (fenced). Returns the snapshot as loaded into this
/// replica's cache. On error the previous publication stays in place.
///
/// Fencing note: `PgLeaseElector` keeps the fence token when the *same*
/// holder re-acquires its own expired lease and only advances it on a
/// change of holder. That is sufficient here because each holder runs
/// exactly one walk at a time from [`run_leader_loop`] (the next
/// `try_acquire` happens only after this function returns), so a stale
/// walk can never race a newer walk of the same holder id; any walk by a
/// different holder carries a strictly newer token and the publication
/// predicate refuses the older one.
pub async fn refresh_dashboard_stats_as(
    state: &AppState,
    leader: &Arc<dyn LeaderElector>,
    lease: Lease,
) -> Result<Arc<StatsSnapshot>, String> {
    let store = state.dashboard.stats_store();
    let fence = LeaseFence {
        lease_name: lease.name.clone(),
        holder_id: lease.holder_id.clone(),
        fence_token: lease.fence_token,
    };
    if !store
        .mark_refresh_started(&fence, state.clock.now())
        .await
        .map_err(|e| e.to_string())?
    {
        return Err("dashboard stats refresher lease is no longer held".to_string());
    }
    let cancel = CancellationToken::new();
    let renewal = spawn_renewal(leader.clone(), lease, cancel.clone());
    // Establish the stored baseline before walking: the previous
    // publication (possibly by another holder, or from before a restart)
    // supplies the decoded vaults to reuse and the reference for the
    // systemic-failure guard. A replica whose sync loop has not run yet
    // must not walk against an empty local cache.
    let previous = match sync_from_store(state).await {
        Ok(previous) => previous,
        Err(error) => {
            cancel.cancel();
            let _ = renewal.await;
            clear_marker(store, &fence).await;
            return Err(format!(
                "load the published baseline before walking: {error}"
            ));
        }
    };
    let started = Instant::now();
    let threshold = (state.storage.kind() == StorageType::Filesystem)
        .then(|| state.dashboard.filesystem_aggregate_threshold());
    let result = build_payload(
        &state.storage,
        &state.metadata,
        &state.clock,
        previous.as_deref(),
        threshold,
        &cancel,
    )
    .await;
    cancel.cancel();
    let _ = renewal.await;
    histogram!(DASHBOARD_STATS_REFRESH_DURATION_SECONDS).record(started.elapsed().as_secs_f64());

    let (as_of, payload) = match result {
        Ok(built) => built,
        Err(error) => {
            counter!(DASHBOARD_STATS_REFRESH_FAILURES_TOTAL).increment(1);
            clear_marker(store, &fence).await;
            return Err(error);
        }
    };
    let json = serde_json::to_value(&payload)
        .map_err(|e| format!("serialize dashboard stats payload: {e}"))?;
    let published = match store
        .publish(&fence, as_of, state.clock.now(), json)
        .await
        .map_err(|e| e.to_string())
    {
        Ok(outcome) => outcome,
        Err(error) => {
            counter!(DASHBOARD_STATS_REFRESH_FAILURES_TOTAL).increment(1);
            clear_marker(store, &fence).await;
            return Err(error);
        }
    };
    match published {
        PublishOutcome::Published {
            version,
            published_at,
        } => {
            let snapshot = Arc::new(StatsSnapshot::new(
                version,
                as_of,
                published_at,
                payload.records,
                payload.inventory,
            ));
            state.dashboard.stats().publish(snapshot.clone());
            gauge!(DASHBOARD_STATS_REFRESH_TIMESTAMP_SECONDS).set(as_of.timestamp() as f64);
            tracing::info!(
                target: "dashboard.stats",
                version,
                accounts = snapshot.accounts.total,
                eligible = snapshot.all_assets.eligible,
                covered = snapshot.all_assets.covered,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "dashboard stats published"
            );
            Ok(snapshot)
        }
        PublishOutcome::StaleLease => {
            counter!(DASHBOARD_STATS_REFRESH_FAILURES_TOTAL).increment(1);
            clear_marker(store, &fence).await;
            Err(
                "dashboard stats publication refused: lease no longer current (expired or superseded)"
                    .to_string(),
            )
        }
    }
}

async fn clear_marker(store: &Arc<dyn crate::coordination::StatsStore>, fence: &LeaseFence) {
    if let Err(error) = store.clear_refresh_started(fence).await {
        tracing::warn!(
            target: "dashboard.stats",
            %error,
            "dashboard stats: could not clear the in-progress marker"
        );
    }
}

/// Single-process convenience (filesystem backend, tests): walk and
/// publish under the always-held lease.
pub async fn refresh_dashboard_stats(state: &AppState) -> Result<Arc<StatsSnapshot>, String> {
    let leader: Arc<dyn LeaderElector> =
        Arc::new(AlwaysLeader::new(DASHBOARD_STATS_LEASE, "single-process"));
    let lease = leader
        .try_acquire(STATS_LEASE_TTL)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "single-process lease unexpectedly unavailable".to_string())?;
    refresh_dashboard_stats_as(state, &leader, lease).await
}

/// Renew the lease while a walk runs; cancels the walk's token when the
/// lease is lost so a superseded holder stops early (publication is
/// fenced regardless).
fn spawn_renewal(
    leader: Arc<dyn LeaderElector>,
    lease: Lease,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(STATS_LEASE_RENEW);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => match leader.renew(&lease, STATS_LEASE_TTL).await {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::warn!(target: "dashboard.stats", "dashboard stats refresher lost its lease mid-walk");
                        cancel.cancel();
                        break;
                    }
                    Err(error) => tracing::warn!(target: "dashboard.stats", %error, "dashboard stats lease renewal failed"),
                },
            }
        }
    })
}

/// Assemble a payload from storage. Pure with respect to the store so
/// tests can drive it directly; `previous` supplies decoded vaults to
/// reuse for unchanged commitments; `filesystem_threshold` is the
/// inventory cap above which the filesystem backend declines the
/// fan-out inventory aggregates. Any systemic storage failure is an
/// `Err` so the caller keeps the previous publication.
pub async fn build_payload(
    storage: &Arc<dyn StorageBackend>,
    metadata: &Arc<dyn MetadataStore>,
    clock: &Arc<dyn Clock>,
    previous: Option<&StatsSnapshot>,
    filesystem_threshold: Option<usize>,
    cancel: &CancellationToken,
) -> Result<(DateTime<Utc>, StatsPayload), String> {
    let as_of = clock.now();
    let listed_ids = metadata
        .list()
        .await
        .map_err(|e| format!("list account metadata: {e}"))?;
    let mut metadatas: HashMap<String, AccountMetadata> = metadata
        .list_all(STATS_PAGE_SIZE)
        .await
        .map_err(|e| format!("read account metadata: {e}"))?
        .into_iter()
        .map(|m| (m.account_id.clone(), m))
        .collect();
    for id in listed_ids {
        if metadatas.contains_key(&id) {
            continue;
        }
        if let Some(m) = metadata
            .get(&id)
            .await
            .map_err(|e| format!("read account metadata '{id}': {e}"))?
        {
            metadatas.insert(id, m);
        }
    }

    // Only successful decodes are carried over: a corrupt or missing
    // row is re-read every walk so a repair is picked up without any
    // commitment change.
    let reusable: HashMap<&str, (&str, &VaultSummary)> = previous
        .map(|p| {
            p.records
                .iter()
                .filter_map(|r| match (&r.state_commitment, &r.vault) {
                    (Some(commitment), VaultOutcome::Decoded { vault }) => {
                        Some((r.account_id.as_str(), (commitment.as_str(), vault)))
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let mut miden_ids: Vec<String> = metadatas
        .values()
        .filter(|m| !m.network_config.is_evm())
        .map(|m| m.account_id.clone())
        .collect();
    miden_ids.sort();

    let mut commitments: HashMap<String, String> = HashMap::with_capacity(miden_ids.len());
    let mut vault_outcomes: HashMap<String, VaultOutcome> = HashMap::with_capacity(miden_ids.len());
    for chunk in miden_ids.chunks(STATS_PAGE_SIZE as usize) {
        if cancel.is_cancelled() {
            return Err("dashboard stats walk cancelled: lease lost".to_string());
        }
        let mut states = read_states_chunk(storage, chunk, &mut vault_outcomes).await?;
        let mut to_decode = Vec::new();
        for id in chunk {
            let Some(state) = states.remove(id) else {
                continue;
            };
            match reusable.get(id.as_str()) {
                Some((commitment, vault)) if *commitment == state.commitment => {
                    vault_outcomes.insert(
                        id.clone(),
                        VaultOutcome::Decoded {
                            vault: (*vault).clone(),
                        },
                    );
                }
                _ => to_decode.push((id.clone(), state.state_json)),
            }
            commitments.insert(id.clone(), state.commitment);
        }
        if to_decode.is_empty() {
            continue;
        }
        let decoded = tokio::task::spawn_blocking(move || {
            to_decode
                .into_iter()
                .map(|(id, state_json)| {
                    let outcome = match decode_vault(&state_json) {
                        Ok(vault) => VaultOutcome::Decoded { vault },
                        Err(error) => {
                            tracing::warn!(
                                target: "dashboard.stats",
                                account_id = %id,
                                %error,
                                "dashboard stats: stored Miden account did not decode"
                            );
                            VaultOutcome::Skipped {
                                reason: SkipReason::StateUndecodable,
                            }
                        }
                    };
                    (id, outcome)
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|e| format!("vault decode task failed: {e}"))?;
        vault_outcomes.extend(decoded);
    }

    // A per-record corruption verdict is only trustworthy in isolation.
    // When every account the previous snapshot had decoded now fails to
    // decode, the cause is almost certainly systemic (wrong key material
    // under the same key id, a broken serializer) and publishing would
    // replace a complete aggregate with an empty one.
    let previously_decoded: Vec<&str> = previous
        .map(|p| {
            p.records
                .iter()
                .filter(|r| matches!(r.vault, VaultOutcome::Decoded { .. }))
                .map(|r| r.account_id.as_str())
                .collect()
        })
        .unwrap_or_default();
    let decoded_now = vault_outcomes
        .values()
        .any(|v| matches!(v, VaultOutcome::Decoded { .. }));
    if !decoded_now
        && !previously_decoded.is_empty()
        && previously_decoded.iter().all(|id| {
            matches!(
                vault_outcomes.get(*id),
                Some(VaultOutcome::Skipped {
                    reason: SkipReason::StateUndecodable
                })
            )
        })
    {
        return Err(format!(
            "every previously decodable account ({}) now fails to decode; treating as a systemic \
             failure and keeping the previous snapshot",
            previously_decoded.len()
        ));
    }

    let records = metadatas
        .into_iter()
        .map(|(id, m)| {
            let vault = if m.network_config.is_evm() {
                VaultOutcome::NotApplicable
            } else {
                vault_outcomes.remove(&id).unwrap_or(VaultOutcome::Skipped {
                    reason: SkipReason::StateUnavailable,
                })
            };
            AccountStatsRecord {
                updated_at: parse_rfc3339_utc(&m.updated_at),
                auth_method: m.auth.method_label().to_string(),
                authorized_signer_count: normalized_authorized_signer_count(&m.auth),
                lifecycle: AccountLifecycle::of(&m),
                state_commitment: commitments.remove(&id),
                account_id: id,
                vault,
            }
        })
        .collect::<Vec<_>>();

    let inventory = read_inventory(storage, records.len(), filesystem_threshold).await;
    Ok((
        as_of,
        StatsPayload {
            schema: STATS_PAYLOAD_SCHEMA,
            records,
            inventory,
        },
    ))
}

/// Batch-read one chunk of states. A failed batch is retried one account
/// at a time: a missing row is `state_unavailable`, a row that does not
/// decrypt is `state_undecodable`, and any other failure is systemic and
/// aborts the walk.
async fn read_states_chunk(
    storage: &Arc<dyn StorageBackend>,
    chunk: &[String],
    vault_outcomes: &mut HashMap<String, VaultOutcome>,
) -> Result<HashMap<String, crate::state_object::StateObject>, String> {
    let refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
    let batch_error = match storage.pull_states_batch(&refs).await {
        Ok(states) => {
            for id in chunk {
                if !states.contains_key(id) {
                    vault_outcomes.insert(
                        id.clone(),
                        VaultOutcome::Skipped {
                            reason: SkipReason::StateUnavailable,
                        },
                    );
                }
            }
            return Ok(states);
        }
        Err(error) => error,
    };
    tracing::debug!(
        target: "dashboard.stats",
        error = %batch_error,
        accounts = chunk.len(),
        "dashboard stats: batched state read failed; classifying accounts individually"
    );
    let mut states = HashMap::with_capacity(chunk.len());
    for id in chunk {
        match storage.pull_state(id).await {
            Ok(state) => {
                states.insert(id.clone(), state);
            }
            Err(error) if is_storage_not_found(&error) => {
                vault_outcomes.insert(
                    id.clone(),
                    VaultOutcome::Skipped {
                        reason: SkipReason::StateUnavailable,
                    },
                );
            }
            Err(error) if is_record_corruption(&error) => {
                tracing::warn!(
                    target: "dashboard.stats",
                    account_id = %id,
                    %error,
                    "dashboard stats: stored state is corrupt; reported as state_undecodable"
                );
                vault_outcomes.insert(
                    id.clone(),
                    VaultOutcome::Skipped {
                        reason: SkipReason::StateUndecodable,
                    },
                );
            }
            Err(error) => {
                return Err(format!(
                    "read account state '{id}' (systemic failure, previous snapshot kept): {error}"
                ));
            }
        }
    }
    Ok(states)
}

/// Inventory aggregates for `/dashboard/info`, each degraded
/// independently on failure or above the filesystem threshold.
async fn read_inventory(
    storage: &Arc<dyn StorageBackend>,
    account_count: usize,
    filesystem_threshold: Option<usize>,
) -> InventoryAggregates {
    let mut inventory = InventoryAggregates::default();
    if filesystem_threshold.is_some_and(|threshold| account_count > threshold) {
        inventory.degraded = vec![
            AGG_DELTA_STATUS_COUNTS.to_string(),
            AGG_IN_FLIGHT_PROPOSAL_COUNT.to_string(),
            AGG_LATEST_ACTIVITY.to_string(),
        ];
        return inventory;
    }
    match storage.count_deltas_by_status().await {
        Ok(counts) => inventory.delta_status_counts = Some(counts.into()),
        Err(error) => {
            tracing::warn!(target: "dashboard.stats", %error, "dashboard stats: count_deltas_by_status failed");
            inventory.degraded.push(AGG_DELTA_STATUS_COUNTS.to_string());
        }
    }
    match storage.count_in_flight_proposals().await {
        Ok(n) => inventory.in_flight_proposal_count = Some(n),
        Err(error) => {
            tracing::warn!(target: "dashboard.stats", %error, "dashboard stats: count_in_flight_proposals failed");
            inventory
                .degraded
                .push(AGG_IN_FLIGHT_PROPOSAL_COUNT.to_string());
        }
    }
    match storage.latest_activity_timestamp().await {
        Ok(ts) => inventory.latest_activity = ts,
        Err(error) => {
            tracing::warn!(target: "dashboard.stats", %error, "dashboard stats: latest_activity_timestamp failed");
            inventory.degraded.push(AGG_LATEST_ACTIVITY.to_string());
        }
    }
    inventory
}

/// Decode the vault totals of a stored Miden account blob. Mirrors the
/// per-account snapshot decode so both surfaces agree on faucet ids.
pub fn decode_vault(state_json: &serde_json::Value) -> Result<VaultSummary, String> {
    let account = miden_protocol::account::Account::from_json(state_json)?;
    let mut summary = VaultSummary::default();
    for asset in account.vault().assets() {
        match asset.as_fungible() {
            Some(a) => {
                let entry = summary.fungible.entry(a.faucet_id().to_hex()).or_insert(0);
                *entry = entry.saturating_add(u64::from(a.amount()));
            }
            None => {
                *summary
                    .non_fungible
                    .entry(asset.faucet_id().to_hex())
                    .or_insert(0) += 1;
            }
        }
    }
    Ok(summary)
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Test-only builders for Miden account states with real vault
/// contents, shared by the stats unit tests and the dashboard API tests.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::state_object::StateObject;
    use guardian_shared::ToJson;
    use miden_protocol::account::{
        Account, AccountCode, AccountId, AccountIdVersion, AccountStorage, AccountType,
        AssetCallbackFlag,
    };
    use miden_protocol::asset::{Asset, AssetVault, FungibleAsset};

    /// Deterministic faucet id from a seed byte.
    pub(crate) fn faucet(seed: u8) -> AccountId {
        AccountId::dummy(
            [seed; 15],
            AccountIdVersion::Version1,
            AccountType::Public,
            AssetCallbackFlag::Disabled,
        )
    }

    /// A private Miden account whose vault holds the given fungible
    /// amounts, serialized the way `/configure` stores it.
    pub(crate) fn miden_state(id: &str, seed: u8, assets: &[(AccountId, u64)]) -> StateObject {
        let account_id = AccountId::dummy(
            [seed; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            AssetCallbackFlag::Disabled,
        );
        let assets: Vec<Asset> = assets
            .iter()
            .map(|(faucet, amount)| {
                FungibleAsset::new(*faucet, *amount)
                    .expect("fungible asset")
                    .into()
            })
            .collect();
        let account = Account::new_existing(
            account_id,
            AssetVault::new(&assets).expect("vault"),
            AccountStorage::new(vec![]).expect("storage"),
            AccountCode::mock(),
            miden_protocol::Felt::new_unchecked(1),
        );
        StateObject {
            account_id: id.to_string(),
            state_json: account.to_json(),
            commitment: format!("0x{}", hex::encode(account.to_commitment().as_bytes())),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            auth_scheme: "falcon".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{faucet, miden_state};
    use super::*;
    use crate::ack::AckRegistry;
    use crate::builder::clock::test::MockClock;
    use crate::coordination::{InMemoryStatsStore, StatsStore};
    use crate::dashboard::DashboardState;
    use crate::metadata::auth::Auth;
    use crate::metadata::{AccountMetadata, NetworkConfig};
    use crate::state_object::StateObject;
    use crate::storage::encryption::cipher::Aes256GcmCipher;
    use crate::storage::encryption::decorator::EncryptedStorage;
    use crate::storage::encryption::key_provider::{InMemoryKeyProvider, StorageKeyProvider};
    use crate::storage::filesystem::FilesystemService;
    use crate::testing::helpers::create_test_app_state;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use base64::Engine as _;

    fn ts(value: &str) -> DateTime<Utc> {
        parse_rfc3339_utc(value).expect("test timestamp")
    }

    fn record(
        id: &str,
        updated_at: Option<&str>,
        auth_method: &str,
        signers: usize,
        lifecycle: AccountLifecycle,
        vault: VaultOutcome,
    ) -> AccountStatsRecord {
        AccountStatsRecord {
            account_id: id.to_string(),
            updated_at: updated_at.map(ts),
            auth_method: auth_method.to_string(),
            authorized_signer_count: signers,
            lifecycle,
            state_commitment: None,
            vault,
        }
    }

    fn decoded(fungible: &[(&str, u64)], non_fungible: &[(&str, u64)]) -> VaultOutcome {
        VaultOutcome::Decoded {
            vault: VaultSummary {
                fungible: fungible.iter().map(|(f, a)| (f.to_string(), *a)).collect(),
                non_fungible: non_fungible
                    .iter()
                    .map(|(f, c)| (f.to_string(), *c))
                    .collect(),
            },
        }
    }

    fn skipped(reason: SkipReason) -> VaultOutcome {
        VaultOutcome::Skipped { reason }
    }

    fn snapshot(as_of: DateTime<Utc>, records: Vec<AccountStatsRecord>) -> StatsSnapshot {
        StatsSnapshot::new(1, as_of, as_of, records, InventoryAggregates::default())
    }

    fn metadata(id: &str, auth: Auth, network: NetworkConfig, updated_at: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: id.to_string(),
            auth,
            network_config: network,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: updated_at.to_string(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    fn falcon(signers: &[&str]) -> Auth {
        Auth::MidenFalconRpo {
            cosigner_commitments: signers.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn evm_network() -> NetworkConfig {
        NetworkConfig::Evm {
            chain_id: 11155111,
            account_address: "0x0000000000000000000000000000000000000001".to_string(),
            multisig_validator_address: "0x0000000000000000000000000000000000000002".to_string(),
        }
    }

    async fn mock_state(metadata: MockMetadataStore, storage: MockStorageBackend) -> AppState {
        mock_state_with_dashboard(metadata, storage, DashboardState::default()).await
    }

    async fn mock_state_with_dashboard(
        metadata: MockMetadataStore,
        storage: MockStorageBackend,
        dashboard: DashboardState,
    ) -> AppState {
        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");
        AppState {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata),
            network_client: Arc::new(MockNetworkClient::new()),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::fixed("2026-09-15T12:00:00Z")),
            dashboard: Arc::new(dashboard),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    async fn build(
        state: &AppState,
        previous: Option<&StatsSnapshot>,
    ) -> Result<(DateTime<Utc>, StatsPayload), String> {
        build_payload(
            &state.storage,
            &state.metadata,
            &state.clock,
            previous,
            None,
            &CancellationToken::new(),
        )
        .await
    }

    fn by_id(payload: &StatsPayload) -> HashMap<&str, &AccountStatsRecord> {
        payload
            .records
            .iter()
            .map(|r| (r.account_id.as_str(), r))
            .collect()
    }

    // --- pure aggregation --------------------------------------------------

    #[test]
    fn lifecycle_released_wins_over_paused_wins_over_active() {
        let mut meta = metadata(
            "a",
            falcon(&["0x1"]),
            NetworkConfig::miden_default(),
            "2026-09-01T00:00:00Z",
        );
        assert_eq!(AccountLifecycle::of(&meta), AccountLifecycle::Active);
        meta.paused_at = Some(ts("2026-09-02T00:00:00Z"));
        assert_eq!(AccountLifecycle::of(&meta), AccountLifecycle::Paused);
        meta.released_at = Some(ts("2026-09-03T00:00:00Z"));
        assert_eq!(AccountLifecycle::of(&meta), AccountLifecycle::Released);
    }

    #[test]
    fn account_counts_bucket_lifecycle_auth_shape_and_activity_windows() {
        let as_of = ts("2026-09-11T00:00:00Z");
        let records = vec![
            record(
                "a",
                Some("2026-09-10T00:00:00Z"),
                "miden_falcon",
                1,
                AccountLifecycle::Active,
                VaultOutcome::NotApplicable,
            ),
            record(
                "b",
                Some("2026-08-20T00:00:00Z"),
                "miden_falcon",
                2,
                AccountLifecycle::Paused,
                VaultOutcome::NotApplicable,
            ),
            // Exactly 7 days old is still "within 7 days" (>= boundary).
            record(
                "c",
                Some("2026-09-04T00:00:00Z"),
                "miden_ecdsa",
                1,
                AccountLifecycle::Released,
                VaultOutcome::NotApplicable,
            ),
            record(
                "d",
                Some("2026-01-01T00:00:00Z"),
                "evm",
                3,
                AccountLifecycle::Active,
                VaultOutcome::NotApplicable,
            ),
            // Unparseable updated_at: counted in totals, never in windows.
            record(
                "e",
                None,
                "miden_falcon",
                1,
                AccountLifecycle::Active,
                VaultOutcome::NotApplicable,
            ),
            // Bumped past as_of while the walk ran: counted in totals, but
            // the windows are anchored to as_of on both sides.
            record(
                "f",
                Some("2026-09-11T00:00:01Z"),
                "miden_falcon",
                1,
                AccountLifecycle::Active,
                VaultOutcome::NotApplicable,
            ),
        ];
        let snapshot = snapshot(as_of, records);
        let counts = &snapshot.accounts;
        assert_eq!(counts.total, 6);
        assert_eq!((counts.active, counts.paused, counts.released), (4, 1, 1));
        assert_eq!(counts.by_auth_method["miden_falcon"], 4);
        assert_eq!(counts.by_auth_method["miden_ecdsa"], 1);
        assert_eq!(counts.by_auth_method["evm"], 1);
        assert_eq!(
            counts.by_auth_method_and_signer_count[&("miden_falcon".to_string(), 1)],
            3
        );
        assert_eq!(
            counts.by_auth_method_and_signer_count[&("miden_falcon".to_string(), 2)],
            1
        );
        assert_eq!(
            counts.by_auth_method_and_signer_count[&("evm".to_string(), 3)],
            1
        );
        assert_eq!(counts.updated_within_7d, 2);
        assert_eq!(counts.updated_within_30d, 3);
        let order: Vec<&str> = snapshot
            .records()
            .iter()
            .map(|r| r.account_id.as_str())
            .collect();
        assert_eq!(order, vec!["f", "a", "c", "b", "d", "e"]);
    }

    #[test]
    fn assets_since_applies_metadata_updated_at_cutoff_to_the_asset_aggregate_only() {
        let as_of = ts("2026-09-11T00:00:00Z");
        let records = vec![
            record(
                "new",
                Some("2026-09-10T00:00:00Z"),
                "miden_falcon",
                1,
                AccountLifecycle::Active,
                decoded(&[("0xf1", 100)], &[("0xn1", 2)]),
            ),
            record(
                "boundary",
                Some("2026-09-04T00:00:00Z"),
                "miden_falcon",
                1,
                AccountLifecycle::Paused,
                decoded(&[("0xf1", 10), ("0xf2", 5)], &[]),
            ),
            record(
                "old",
                Some("2026-01-01T00:00:00Z"),
                "miden_falcon",
                1,
                AccountLifecycle::Active,
                decoded(&[("0xf1", 1)], &[("0xn1", 1)]),
            ),
            record(
                "unparseable",
                None,
                "miden_falcon",
                1,
                AccountLifecycle::Active,
                decoded(&[("0xf1", 1000)], &[]),
            ),
        ];
        let snapshot = snapshot(as_of, records);

        let all = snapshot.assets_since(None);
        assert_eq!((all.eligible, all.covered), (4, 4));
        assert!(all.complete());
        assert_eq!(all.fungible["0xf1"], 1111);
        assert_eq!(all.fungible["0xf2"], 5);
        assert_eq!(all.non_fungible["0xn1"], 3);

        // Inclusive cutoff: the account updated exactly at `since` counts.
        let since = snapshot.assets_since(Some(ts("2026-09-04T00:00:00Z")));
        assert_eq!((since.eligible, since.covered), (2, 2));
        assert_eq!(since.fungible["0xf1"], 110);
        assert_eq!(since.fungible["0xf2"], 5);
        assert_eq!(since.non_fungible["0xn1"], 2);

        let none = snapshot.assets_since(Some(ts("2026-09-11T00:00:00Z")));
        assert_eq!((none.eligible, none.covered), (0, 0));
        assert!(none.complete());
        assert!(none.fungible.is_empty());
        assert_eq!(snapshot.accounts.total, 4);
    }

    /// The filtered query folds at most one prefix block: check every
    /// cutoff against a naive fold over an inventory spanning several
    /// blocks, including exact block boundaries.
    #[test]
    fn assets_since_matches_a_naive_fold_across_prefix_blocks() {
        let as_of = ts("2026-09-11T00:00:00Z");
        let n = PREFIX_BLOCK * 3 + 17;
        let records: Vec<AccountStatsRecord> = (0..n)
            .map(|i| {
                let updated = as_of - chrono::Duration::minutes(i as i64);
                let mut r = record(
                    &format!("acc-{i:05}"),
                    None,
                    "miden_falcon",
                    1,
                    AccountLifecycle::Active,
                    if i % 7 == 0 {
                        skipped(SkipReason::StateUnavailable)
                    } else {
                        decoded(
                            &[
                                ("0xf1", i as u64),
                                (if i % 2 == 0 { "0xeven" } else { "0xodd" }, 1),
                            ],
                            &[("0xn", 1)],
                        )
                    },
                );
                r.updated_at = Some(updated);
                r
            })
            .collect();
        let snapshot = snapshot(as_of, records);
        for cut in [
            0usize,
            1,
            255,
            256,
            257,
            511,
            512,
            513,
            700,
            768,
            769,
            n - 1,
            n,
            n + 5,
        ] {
            let since = as_of - chrono::Duration::minutes(cut as i64);
            let fast = snapshot.assets_since(Some(since));
            let mut naive = AssetAggregate::default();
            for r in snapshot
                .records()
                .iter()
                .filter(|r| r.updated_at.unwrap() >= since)
            {
                naive.add(r);
            }
            assert_eq!(fast, naive, "cutoff at {cut} records");
        }
    }

    #[test]
    fn fungible_totals_widen_beyond_u64() {
        let records = vec![
            record(
                "a",
                Some("2026-09-10T00:00:00Z"),
                "miden_falcon",
                1,
                AccountLifecycle::Active,
                decoded(&[("0xf1", u64::MAX)], &[]),
            ),
            record(
                "b",
                Some("2026-09-10T00:00:00Z"),
                "miden_falcon",
                1,
                AccountLifecycle::Active,
                decoded(&[("0xf1", u64::MAX)], &[]),
            ),
        ];
        let all = snapshot(ts("2026-09-11T00:00:00Z"), records).assets_since(None);
        assert_eq!(all.fungible["0xf1"], u128::from(u64::MAX) * 2);
        assert_eq!(all.fungible["0xf1"].to_string(), "36893488147419103230");
    }

    #[test]
    fn skipped_accounts_are_counted_by_reason_and_make_the_aggregate_incomplete() {
        let records = vec![
            record(
                "ok",
                Some("2026-09-10T00:00:00Z"),
                "miden_falcon",
                1,
                AccountLifecycle::Active,
                decoded(&[("0xf1", 100)], &[]),
            ),
            record(
                "missing",
                Some("2026-09-10T00:00:00Z"),
                "miden_falcon",
                1,
                AccountLifecycle::Active,
                skipped(SkipReason::StateUnavailable),
            ),
            record(
                "garbage",
                Some("2026-09-10T00:00:00Z"),
                "miden_ecdsa",
                1,
                AccountLifecycle::Active,
                skipped(SkipReason::StateUndecodable),
            ),
            record(
                "missing2",
                Some("2026-09-10T00:00:00Z"),
                "miden_falcon",
                1,
                AccountLifecycle::Active,
                skipped(SkipReason::StateUnavailable),
            ),
            record(
                "evm",
                Some("2026-09-10T00:00:00Z"),
                "evm",
                1,
                AccountLifecycle::Active,
                VaultOutcome::NotApplicable,
            ),
        ];
        let all = snapshot(ts("2026-09-11T00:00:00Z"), records).assets_since(None);
        assert_eq!(all.eligible, 4);
        assert_eq!(all.covered, 1);
        assert_eq!(all.skipped[&SkipReason::StateUnavailable], 2);
        assert_eq!(all.skipped[&SkipReason::StateUndecodable], 1);
        assert_eq!(all.covered + all.skipped_total(), all.eligible);
        assert!(!all.complete());
        assert_eq!(all.fungible.len(), 1);
        assert_eq!(all.fungible["0xf1"], 100);
    }

    #[test]
    fn payload_round_trips_through_json_and_rejects_unknown_schema() {
        let as_of = ts("2026-09-11T00:00:00Z");
        let mut r = record(
            "a",
            Some("2026-09-10T00:00:00Z"),
            "miden_falcon",
            2,
            AccountLifecycle::Paused,
            decoded(&[("0xf1", 5)], &[("0xn", 2)]),
        );
        r.state_commitment = Some("0xc".into());
        let payload = StatsPayload {
            schema: STATS_PAYLOAD_SCHEMA,
            records: vec![
                r,
                record(
                    "b",
                    None,
                    "evm",
                    1,
                    AccountLifecycle::Active,
                    VaultOutcome::NotApplicable,
                ),
                record(
                    "c",
                    Some("2026-09-10T00:00:00Z"),
                    "miden_ecdsa",
                    1,
                    AccountLifecycle::Released,
                    skipped(SkipReason::StateUndecodable),
                ),
            ],
            inventory: InventoryAggregates {
                delta_status_counts: Some(SnapshotDeltaCounts {
                    candidate: 1,
                    canonical: 2,
                    retained: 3,
                    discarded: 4,
                }),
                in_flight_proposal_count: Some(9),
                latest_activity: Some(as_of),
                degraded: vec![],
            },
        };
        let published = PublishedStats {
            version: 4,
            as_of,
            published_at: as_of,
            payload: serde_json::to_value(&payload).unwrap(),
        };
        let decoded_snapshot = StatsSnapshot::from_published(&published).unwrap();
        assert_eq!(decoded_snapshot.version, 4);
        assert_eq!(decoded_snapshot.accounts.total, 3);
        assert_eq!(decoded_snapshot.inventory, payload.inventory);
        let mut sorted = payload.records.clone();
        sorted.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.account_id.cmp(&b.account_id))
        });
        assert_eq!(decoded_snapshot.records(), sorted.as_slice());

        let mut newer = published.clone();
        newer.payload["schema"] = serde_json::json!(STATS_PAYLOAD_SCHEMA + 1);
        assert!(StatsSnapshot::from_published(&newer).is_err());
    }

    #[test]
    fn decode_vault_rejects_garbage_and_sums_real_assets() {
        assert!(decode_vault(&serde_json::json!({ "data": "not-base64!" })).is_err());
        let f1 = faucet(0x11);
        let f2 = faucet(0x22);
        let state = miden_state("acc", 0x01, &[(f1, 1_000), (f2, 5)]);
        let vault = decode_vault(&state.state_json).expect("decodes");
        assert_eq!(vault.fungible[&f1.to_hex()], 1_000);
        assert_eq!(vault.fungible[&f2.to_hex()], 5);
        assert!(vault.non_fungible.is_empty());
    }

    // --- the walk ------------------------------------------------------------

    #[tokio::test]
    async fn build_payload_walks_filesystem_backends_and_classifies_every_account() {
        let state = create_test_app_state().await;
        let f1 = faucet(0x11);
        let f2 = faucet(0x22);
        let recent = "2026-09-10T00:00:00Z";
        let stale = "2026-01-01T00:00:00Z";

        let a = miden_state("acc-a", 0x01, &[(f1, 1_000), (f2, 7)]);
        state
            .metadata
            .set(metadata(
                "acc-a",
                falcon(&["0x1", "0x2"]),
                NetworkConfig::miden_default(),
                recent,
            ))
            .await
            .unwrap();
        state.storage.submit_state(&a).await.unwrap();
        let b = miden_state("acc-b", 0x02, &[(f1, 500)]);
        let mut b_meta = metadata(
            "acc-b",
            falcon(&["0x1"]),
            NetworkConfig::miden_default(),
            stale,
        );
        b_meta.paused_at = Some(ts("2026-02-01T00:00:00Z"));
        b_meta.paused_reason = Some("test".to_string());
        state.metadata.set(b_meta).await.unwrap();
        state.storage.submit_state(&b).await.unwrap();
        let mut c_meta = metadata(
            "acc-c",
            Auth::MidenEcdsa {
                cosigner_commitments: vec!["0x9".to_string()],
            },
            NetworkConfig::miden_default(),
            recent,
        );
        c_meta.released_at = Some(ts("2026-09-01T00:00:00Z"));
        state.metadata.set(c_meta).await.unwrap();
        state
            .storage
            .submit_state(&StateObject {
                account_id: "acc-c".to_string(),
                state_json: serde_json::json!({ "data": "!!not-base64!!" }),
                commitment: "0xc".to_string(),
                created_at: recent.to_string(),
                updated_at: recent.to_string(),
                auth_scheme: "ecdsa".to_string(),
            })
            .await
            .unwrap();
        state
            .metadata
            .set(metadata(
                "acc-d",
                falcon(&["0x1"]),
                NetworkConfig::miden_default(),
                recent,
            ))
            .await
            .unwrap();
        state
            .metadata
            .set(metadata(
                "acc-e",
                Auth::EvmEcdsa {
                    signers: vec!["0xe1".into(), "0xe2".into(), "0xe3".into()],
                },
                evm_network(),
                recent,
            ))
            .await
            .unwrap();

        let (as_of, payload) = build(&state, None).await.expect("payload");
        // `create_test_app_state` runs on the system clock: `as_of` is
        // the walk's start, so it cannot be later than "now".
        assert!(as_of <= state.clock.now());
        let snapshot = StatsSnapshot::new(
            1,
            as_of,
            as_of,
            payload.records.clone(),
            payload.inventory.clone(),
        );

        let counts = &snapshot.accounts;
        assert_eq!(counts.total, 5);
        assert_eq!((counts.active, counts.paused, counts.released), (3, 1, 1));
        assert_eq!(counts.by_auth_method["miden_falcon"], 3);
        assert_eq!(counts.by_auth_method["miden_ecdsa"], 1);
        assert_eq!(counts.by_auth_method["evm"], 1);

        let ids = by_id(&payload);
        assert!(matches!(ids["acc-a"].vault, VaultOutcome::Decoded { .. }));
        assert_eq!(
            ids["acc-a"].state_commitment.as_deref(),
            Some(a.commitment.as_str())
        );
        assert!(matches!(ids["acc-b"].vault, VaultOutcome::Decoded { .. }));
        assert_eq!(ids["acc-c"].vault, skipped(SkipReason::StateUndecodable));
        assert_eq!(ids["acc-c"].state_commitment.as_deref(), Some("0xc"));
        assert_eq!(ids["acc-d"].vault, skipped(SkipReason::StateUnavailable));
        assert_eq!(ids["acc-d"].state_commitment, None);
        assert_eq!(ids["acc-e"].vault, VaultOutcome::NotApplicable);

        let all = snapshot.assets_since(None);
        assert_eq!((all.eligible, all.covered), (4, 2));
        assert_eq!(all.skipped[&SkipReason::StateUndecodable], 1);
        assert_eq!(all.skipped[&SkipReason::StateUnavailable], 1);
        assert!(!all.complete());
        assert_eq!(all.fungible[&f1.to_hex()], 1_500);
        assert_eq!(all.fungible[&f2.to_hex()], 7);
        let since = snapshot.assets_since(Some(ts("2026-09-01T00:00:00Z")));
        assert_eq!((since.eligible, since.covered), (3, 1));
        assert_eq!(since.fungible[&f1.to_hex()], 1_000);

        // Small filesystem inventory: the fan-out aggregates were computed.
        assert!(payload.inventory.degraded.is_empty());
        assert_eq!(
            payload.inventory.delta_status_counts,
            Some(SnapshotDeltaCounts::default())
        );
        assert_eq!(payload.inventory.in_flight_proposal_count, Some(0));
    }

    #[tokio::test]
    async fn build_payload_applies_the_filesystem_inventory_threshold_to_fanout_aggregates() {
        let state = create_test_app_state().await;
        for i in 0..3 {
            state
                .metadata
                .set(metadata(
                    &format!("acc-{i}"),
                    falcon(&["0x1"]),
                    NetworkConfig::miden_default(),
                    "2026-09-10T00:00:00Z",
                ))
                .await
                .unwrap();
        }
        let (_, payload) = build_payload(
            &state.storage,
            &state.metadata,
            &state.clock,
            None,
            Some(2),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            payload.inventory.degraded,
            vec![
                AGG_DELTA_STATUS_COUNTS.to_string(),
                AGG_IN_FLIGHT_PROPOSAL_COUNT.to_string(),
                AGG_LATEST_ACTIVITY.to_string()
            ]
        );
        assert_eq!(payload.inventory.delta_status_counts, None);
        // Account and asset records are still complete above the threshold.
        assert_eq!(payload.records.len(), 3);
    }

    #[tokio::test]
    async fn build_payload_reuses_decoded_vaults_only_and_recovers_repaired_blobs() {
        let state = create_test_app_state().await;
        let f1 = faucet(0x11);
        let a = miden_state("acc-a", 0x01, &[(f1, 1_000)]);
        state
            .metadata
            .set(metadata(
                "acc-a",
                falcon(&["0x1"]),
                NetworkConfig::miden_default(),
                "2026-09-10T00:00:00Z",
            ))
            .await
            .unwrap();
        state.storage.submit_state(&a).await.unwrap();
        // acc-g: stored blob is valid, but the previous snapshot recorded
        // it as undecodable under the same commitment (a repair since).
        let mut g = miden_state("acc-g", 0x07, &[(f1, 3)]);
        g.commitment = "0xsame".to_string();
        state
            .metadata
            .set(metadata(
                "acc-g",
                falcon(&["0x1"]),
                NetworkConfig::miden_default(),
                "2026-09-10T00:00:00Z",
            ))
            .await
            .unwrap();
        state.storage.submit_state(&g).await.unwrap();

        let sentinel = decoded(&[("0xsentinel", 7)], &[]);
        let mut prev_a = record(
            "acc-a",
            Some("2026-09-10T00:00:00Z"),
            "miden_falcon",
            1,
            AccountLifecycle::Active,
            sentinel.clone(),
        );
        prev_a.state_commitment = Some(a.commitment.clone());
        let mut prev_g = record(
            "acc-g",
            Some("2026-09-10T00:00:00Z"),
            "miden_falcon",
            1,
            AccountLifecycle::Active,
            skipped(SkipReason::StateUndecodable),
        );
        prev_g.state_commitment = Some("0xsame".to_string());
        let previous = snapshot(ts("2026-09-11T00:00:00Z"), vec![prev_a, prev_g]);

        let (_, payload) = build(&state, Some(&previous)).await.unwrap();
        let ids = by_id(&payload);
        assert_eq!(
            ids["acc-a"].vault, sentinel,
            "unchanged commitment reuses the decoded vault"
        );
        match &ids["acc-g"].vault {
            VaultOutcome::Decoded { vault } => assert_eq!(vault.fungible[&f1.to_hex()], 3),
            other => panic!("repaired blob must be re-decoded, got {other:?}"),
        }

        // A changed commitment forces a fresh decode.
        let mut stale_a = record(
            "acc-a",
            Some("2026-09-10T00:00:00Z"),
            "miden_falcon",
            1,
            AccountLifecycle::Active,
            sentinel,
        );
        stale_a.state_commitment = Some("0xolder".to_string());
        let (_, payload) = build(
            &state,
            Some(&snapshot(ts("2026-09-11T00:00:00Z"), vec![stale_a])),
        )
        .await
        .unwrap();
        match &by_id(&payload)["acc-a"].vault {
            VaultOutcome::Decoded { vault } => {
                assert_eq!(vault.fungible[&f1.to_hex()], 1_000);
                assert!(!vault.fungible.contains_key("0xsentinel"));
            }
            other => panic!("expected decoded vault, got {other:?}"),
        }
    }

    /// Every account the previous snapshot decoded now fails to decode:
    /// that is a systemic failure (wrong key under the same id, broken
    /// serializer), not per-record corruption, so the walk fails and the
    /// previous snapshot stays published.
    #[tokio::test]
    async fn build_payload_fails_when_every_previously_decoded_account_becomes_undecodable() {
        let state = create_test_app_state().await;
        for id in ["acc-a", "acc-b"] {
            state
                .metadata
                .set(metadata(
                    id,
                    falcon(&["0x1"]),
                    NetworkConfig::miden_default(),
                    "2026-09-10T00:00:00Z",
                ))
                .await
                .unwrap();
            state
                .storage
                .submit_state(&StateObject {
                    account_id: id.to_string(),
                    state_json: serde_json::json!({ "data": "!!garbage!!" }),
                    commitment: format!("0xnew-{id}"),
                    created_at: "2026-09-10T00:00:00Z".to_string(),
                    updated_at: "2026-09-10T00:00:00Z".to_string(),
                    auth_scheme: "falcon".to_string(),
                })
                .await
                .unwrap();
        }
        // No previous snapshot: explicit coverage, the walk succeeds.
        let (_, payload) = build(&state, None).await.unwrap();
        assert!(
            payload
                .records
                .iter()
                .all(|r| r.vault == skipped(SkipReason::StateUndecodable))
        );

        // Previous snapshot had both decoded: now systemic.
        let mut prev_a = record(
            "acc-a",
            Some("2026-09-10T00:00:00Z"),
            "miden_falcon",
            1,
            AccountLifecycle::Active,
            decoded(&[("0xf1", 1)], &[]),
        );
        prev_a.state_commitment = Some("0xold-a".into());
        let mut prev_b = record(
            "acc-b",
            Some("2026-09-10T00:00:00Z"),
            "miden_falcon",
            1,
            AccountLifecycle::Active,
            decoded(&[("0xf1", 1)], &[]),
        );
        prev_b.state_commitment = Some("0xold-b".into());
        let err = build(
            &state,
            Some(&snapshot(ts("2026-09-11T00:00:00Z"), vec![prev_a, prev_b])),
        )
        .await
        .unwrap_err();
        assert!(err.contains("systemic"), "{err}");

        // One of them decodes again: back to per-record coverage.
        let f1 = faucet(0x11);
        state
            .storage
            .submit_state(&miden_state("acc-a", 0x01, &[(f1, 5)]))
            .await
            .unwrap();
        let mut prev_a = record(
            "acc-a",
            Some("2026-09-10T00:00:00Z"),
            "miden_falcon",
            1,
            AccountLifecycle::Active,
            decoded(&[("0xf1", 1)], &[]),
        );
        prev_a.state_commitment = Some("0xold-a".into());
        let mut prev_b = record(
            "acc-b",
            Some("2026-09-10T00:00:00Z"),
            "miden_falcon",
            1,
            AccountLifecycle::Active,
            decoded(&[("0xf1", 1)], &[]),
        );
        prev_b.state_commitment = Some("0xold-b".into());
        let (_, payload) = build(
            &state,
            Some(&snapshot(ts("2026-09-11T00:00:00Z"), vec![prev_a, prev_b])),
        )
        .await
        .unwrap();
        let ids = by_id(&payload);
        assert!(matches!(ids["acc-a"].vault, VaultOutcome::Decoded { .. }));
        assert_eq!(ids["acc-b"].vault, skipped(SkipReason::StateUndecodable));
    }

    #[tokio::test]
    async fn build_payload_recovers_accounts_the_cursor_walk_missed() {
        let a = metadata(
            "a",
            Auth::EvmEcdsa {
                signers: vec!["0x1".to_string()],
            },
            evm_network(),
            "2026-09-10T00:00:00Z",
        );
        let b = metadata(
            "b",
            Auth::EvmEcdsa {
                signers: vec!["0x1".to_string()],
            },
            evm_network(),
            "2026-09-09T00:00:00Z",
        );
        let metadata_store = MockMetadataStore::new()
            .with_list(Ok(vec!["a".to_string(), "b".to_string()]))
            .with_list_paged(Ok(vec![a]))
            .with_get(Ok(Some(b)));
        let state = mock_state(metadata_store, MockStorageBackend::new()).await;
        let (_, payload) = build(&state, None).await.unwrap();
        let mut ids: Vec<&str> = payload
            .records
            .iter()
            .map(|r| r.account_id.as_str())
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn build_payload_treats_a_storage_fault_as_systemic_and_fails() {
        let meta = metadata(
            "a",
            falcon(&["0x1"]),
            NetworkConfig::miden_default(),
            "2026-09-10T00:00:00Z",
        );
        let metadata_store = MockMetadataStore::new()
            .with_list(Ok(vec!["a".to_string()]))
            .with_list_paged(Ok(vec![meta]));
        // The batch read fails with a non-missing error, and so does the
        // per-account fallback: that is a storage fault, not a missing
        // row, so the walk must fail instead of publishing reduced totals.
        let storage = MockStorageBackend::new()
            .with_pull_state(Err("db down".to_string()))
            .with_pull_state(Err("db down".to_string()));
        let state = mock_state(metadata_store, storage).await;
        let err = build(&state, None).await.unwrap_err();
        assert!(err.contains("systemic failure"), "{err}");
    }

    #[tokio::test]
    async fn build_payload_reports_a_missing_state_row_as_unavailable() {
        let meta = metadata(
            "a",
            falcon(&["0x1"]),
            NetworkConfig::miden_default(),
            "2026-09-10T00:00:00Z",
        );
        let metadata_store = MockMetadataStore::new()
            .with_list(Ok(vec!["a".to_string()]))
            .with_list_paged(Ok(vec![meta]));
        let storage = MockStorageBackend::new().with_pull_state(Err("state not found".to_string()));
        let state = mock_state(metadata_store, storage).await;
        let (_, payload) = build(&state, None).await.unwrap();
        assert_eq!(
            by_id(&payload)["a"].vault,
            skipped(SkipReason::StateUnavailable)
        );
    }

    fn provider(byte: u8, kid: &str) -> Arc<dyn StorageKeyProvider> {
        let key = base64::engine::general_purpose::STANDARD.encode([byte; 32]);
        Arc::new(InMemoryKeyProvider::from_dev_key(&key, kid).unwrap())
    }

    /// Encrypted storage: one row that does not decrypt is explicit
    /// `state_undecodable` coverage — including a row sealed under a
    /// retired key id — while "nothing decodes any more" against a
    /// previously decodable inventory is systemic and fails the walk.
    #[tokio::test]
    async fn build_payload_distinguishes_a_corrupt_encrypted_row_from_a_systemic_key_failure() {
        let dir = tempfile::tempdir().unwrap();
        let plain: Arc<dyn StorageBackend> = Arc::new(
            FilesystemService::new(dir.path().to_path_buf())
                .await
                .unwrap(),
        );
        let encrypted: Arc<dyn StorageBackend> = Arc::new(EncryptedStorage::new(
            plain.clone(),
            Arc::new(Aes256GcmCipher::new(provider(3, "k1"))),
        ));
        let f1 = faucet(0x11);
        encrypted
            .submit_state(&miden_state("acc-ok", 0x01, &[(f1, 10)]))
            .await
            .unwrap();
        encrypted
            .submit_state(&miden_state("acc-bad", 0x02, &[(f1, 20)]))
            .await
            .unwrap();
        // Corrupt acc-bad at rest: overwrite the envelope with a plain blob.
        plain
            .submit_state(&StateObject {
                account_id: "acc-bad".to_string(),
                state_json: serde_json::json!({ "data": "plain, not an envelope" }),
                commitment: "0xbad".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                auth_scheme: "falcon".to_string(),
            })
            .await
            .unwrap();

        let metadata_store = MockMetadataStore::new()
            .with_list(Ok(vec!["acc-ok".to_string(), "acc-bad".to_string()]))
            .with_list_paged(Ok(vec![
                metadata(
                    "acc-ok",
                    falcon(&["0x1"]),
                    NetworkConfig::miden_default(),
                    "2026-09-10T00:00:00Z",
                ),
                metadata(
                    "acc-bad",
                    falcon(&["0x1"]),
                    NetworkConfig::miden_default(),
                    "2026-09-10T00:00:00Z",
                ),
            ]));
        let metadata_store: Arc<dyn MetadataStore> = Arc::new(metadata_store);
        let clock: Arc<dyn Clock> = Arc::new(MockClock::fixed("2026-09-15T12:00:00Z"));

        let (_, payload) = build_payload(
            &encrypted,
            &metadata_store,
            &clock,
            None,
            None,
            &CancellationToken::new(),
        )
        .await
        .expect("one corrupt row must not fail the walk");
        let ids = by_id(&payload);
        assert!(matches!(ids["acc-ok"].vault, VaultOutcome::Decoded { .. }));
        assert_eq!(ids["acc-bad"].vault, skipped(SkipReason::StateUndecodable));

        // Same rows read through a provider that does not know key `k1`:
        // every row fails on the key provider, which is systemic.
        let wrong_key: Arc<dyn StorageBackend> = Arc::new(EncryptedStorage::new(
            plain,
            Arc::new(Aes256GcmCipher::new(provider(3, "k2"))),
        ));
        let metadata_store: Arc<dyn MetadataStore> = Arc::new(
            MockMetadataStore::new()
                .with_list(Ok(vec!["acc-ok".to_string()]))
                .with_list(Ok(vec!["acc-ok".to_string()]))
                .with_list_paged(Ok(vec![metadata(
                    "acc-ok",
                    falcon(&["0x1"]),
                    NetworkConfig::miden_default(),
                    "2026-09-10T00:00:00Z",
                )]))
                .with_list_paged(Ok(vec![metadata(
                    "acc-ok",
                    falcon(&["0x1"]),
                    NetworkConfig::miden_default(),
                    "2026-09-10T00:00:00Z",
                )])),
        );
        // With no previous publication the row is reported as explicit
        // `state_undecodable` coverage (a retired key id is a per-record
        // verdict) ...
        let (_, payload) = build_payload(
            &wrong_key,
            &metadata_store,
            &clock,
            None,
            None,
            &CancellationToken::new(),
        )
        .await
        .expect("a single row under a retired key id is explicit coverage");
        assert_eq!(
            by_id(&payload)["acc-ok"].vault,
            skipped(SkipReason::StateUndecodable)
        );
        // ... but when the previous publication decoded that row and
        // nothing decodes now, the walk is escalated to a systemic
        // failure so the good snapshot stays published.
        let mut prev = record(
            "acc-ok",
            Some("2026-09-10T00:00:00Z"),
            "miden_falcon",
            1,
            AccountLifecycle::Active,
            decoded(&[("0xf1", 1)], &[]),
        );
        prev.state_commitment = Some("0xold".into());
        let err = build_payload(
            &wrong_key,
            &metadata_store,
            &clock,
            Some(&snapshot(ts("2026-09-11T00:00:00Z"), vec![prev])),
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("systemic"), "{err}");
    }

    // --- publication, leadership, and sync ---------------------------------------

    #[tokio::test]
    async fn refresh_publishes_to_the_shared_store_and_followers_sync_the_same_version() {
        let store: Arc<dyn StatsStore> = Arc::new(InMemoryStatsStore::new());
        let replica_a = mock_state_with_dashboard(
            MockMetadataStore::new()
                .with_list(Ok(vec!["a".to_string()]))
                .with_list_paged(Ok(vec![metadata(
                    "a",
                    Auth::EvmEcdsa {
                        signers: vec!["0x1".into()],
                    },
                    evm_network(),
                    "2026-09-10T00:00:00Z",
                )])),
            MockStorageBackend::new(),
            DashboardState::for_tests_with_stats_store(Vec::new(), store.clone()),
        )
        .await;
        let replica_b = mock_state_with_dashboard(
            MockMetadataStore::new(),
            MockStorageBackend::new(),
            DashboardState::for_tests_with_stats_store(Vec::new(), store.clone()),
        )
        .await;

        assert_eq!(
            sync_from_store(&replica_b).await.unwrap(),
            None,
            "nothing published yet"
        );
        let published = refresh_dashboard_stats(&replica_a).await.expect("publish");
        assert_eq!(published.version, 1);
        assert_eq!(published.accounts.total, 1);
        assert!(Arc::ptr_eq(
            &published,
            &replica_a.dashboard.stats().current().unwrap()
        ));

        let synced = sync_from_store(&replica_b)
            .await
            .unwrap()
            .expect("follower loads it");
        assert_eq!(synced.version, 1);
        assert_eq!(synced.as_of, published.as_of);
        assert_eq!(synced.accounts, published.accounts);
        // Unchanged head: the follower keeps its loaded copy.
        let again = sync_from_store(&replica_b).await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&again, &synced));
    }

    #[tokio::test]
    async fn failed_walk_keeps_the_previous_publication_and_clears_its_marker() {
        let store: Arc<dyn StatsStore> = Arc::new(InMemoryStatsStore::new());
        let state = mock_state_with_dashboard(
            MockMetadataStore::new()
                // LIFO: first refresh sees an empty inventory, second fails.
                .with_list(Err("metadata store unreachable".to_string()))
                .with_list(Ok(vec![])),
            MockStorageBackend::new(),
            DashboardState::for_tests_with_stats_store(Vec::new(), store.clone()),
        )
        .await;
        let first = refresh_dashboard_stats(&state)
            .await
            .expect("first refresh");
        let err = refresh_dashboard_stats(&state).await.unwrap_err();
        assert!(err.contains("metadata store unreachable"));
        assert!(Arc::ptr_eq(
            &first,
            &state.dashboard.stats().current().unwrap()
        ));
        let control = store.read_control(state.clock.now()).await.unwrap();
        assert_eq!(
            control.refresh_started_at, None,
            "failed walk clears the in-progress marker"
        );
        assert_eq!(store.current_version().await.unwrap(), Some(1));
    }

    #[tokio::test]
    async fn superseded_holder_cannot_overwrite_a_newer_publication() {
        let store: Arc<dyn StatsStore> = Arc::new(InMemoryStatsStore::new());
        let state = mock_state_with_dashboard(
            MockMetadataStore::new(),
            MockStorageBackend::new(),
            DashboardState::for_tests_with_stats_store(Vec::new(), store.clone()),
        )
        .await;
        // A newer holder (fence token 9) already published.
        let newer = LeaseFence {
            lease_name: DASHBOARD_STATS_LEASE.into(),
            holder_id: "replica-z".into(),
            fence_token: 9,
        };
        let now = state.clock.now();
        store
            .publish(
                &newer,
                now,
                now,
                serde_json::to_value(StatsPayload {
                    schema: STATS_PAYLOAD_SCHEMA,
                    records: vec![],
                    inventory: InventoryAggregates::default(),
                })
                .unwrap(),
            )
            .await
            .unwrap();
        // The single-process lease carries fence token 0: refused. The
        // walk synced the newer publication as its baseline first, and
        // that is what stays loaded.
        let err = refresh_dashboard_stats(&state).await.unwrap_err();
        assert!(err.contains("no longer current"), "{err}");
        assert_eq!(store.current_version().await.unwrap(), Some(1));
        assert_eq!(
            state.dashboard.stats().current().map(|s| s.version),
            Some(1),
            "the refused walk must not replace the synced baseline"
        );
    }

    /// The leader loop publishes immediately, does not walk again while
    /// nothing is due, and walks again when an operator requests it.
    #[tokio::test(start_paused = true)]
    async fn leader_loop_publishes_immediately_and_honors_operator_requests() {
        let store: Arc<dyn StatsStore> = Arc::new(InMemoryStatsStore::new());
        let state = mock_state_with_dashboard(
            MockMetadataStore::new(),
            MockStorageBackend::new(),
            DashboardState::for_tests_with_stats_store(Vec::new(), store.clone()),
        )
        .await;
        let leader: Arc<dyn LeaderElector> =
            Arc::new(AlwaysLeader::new(DASHBOARD_STATS_LEASE, "single-process"));
        let task = tokio::spawn(run_leader_loop(state.clone(), leader));

        async fn wait_for_version(store: &Arc<dyn StatsStore>, expected: i64) {
            for _ in 0..500 {
                if store.current_version().await.unwrap() == Some(expected) {
                    return;
                }
                tokio::task::yield_now().await;
            }
            panic!("store never reached version {expected}");
        }

        wait_for_version(&store, 1).await;
        // Several ticks with a fixed clock: nothing is due, no new version.
        for _ in 0..3 {
            tokio::time::advance(STATS_TICK).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(store.current_version().await.unwrap(), Some(1));

        // An operator request makes the next tick walk again.
        store
            .request_refresh(
                "op",
                state.clock.now(),
                STATS_REFRESH_COOLDOWN,
                STATS_REFRESH_STALE_AFTER,
            )
            .await
            .unwrap();
        tokio::time::advance(STATS_TICK).await;
        wait_for_version(&store, 2).await;
        let control = store.read_control(state.clock.now()).await.unwrap();
        assert_eq!(
            control.refresh_requested_at, None,
            "publication consumed the request"
        );
        task.abort();
    }

    /// A store that can be made to report "no publication" so the
    /// follower's cache-clearing path is observable without Postgres.
    struct VanishingStore {
        inner: InMemoryStatsStore,
        vanished: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl StatsStore for VanishingStore {
        async fn current_version(&self) -> crate::error::Result<Option<i64>> {
            if self.vanished.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(None);
            }
            self.inner.current_version().await
        }
        async fn load_current(&self) -> crate::error::Result<Option<PublishedStats>> {
            if self.vanished.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(None);
            }
            self.inner.load_current().await
        }
        async fn read_control(
            &self,
            now: DateTime<Utc>,
        ) -> crate::error::Result<crate::coordination::StatsControl> {
            self.inner.read_control(now).await
        }
        async fn mark_refresh_started(
            &self,
            fence: &LeaseFence,
            now: DateTime<Utc>,
        ) -> crate::error::Result<bool> {
            self.inner.mark_refresh_started(fence, now).await
        }
        async fn clear_refresh_started(&self, fence: &LeaseFence) -> crate::error::Result<()> {
            self.inner.clear_refresh_started(fence).await
        }
        async fn publish(
            &self,
            fence: &LeaseFence,
            as_of: DateTime<Utc>,
            now: DateTime<Utc>,
            payload: serde_json::Value,
        ) -> crate::error::Result<PublishOutcome> {
            self.inner.publish(fence, as_of, now, payload).await
        }
        async fn request_refresh(
            &self,
            requested_by: &str,
            now: DateTime<Utc>,
            cooldown: Duration,
            stale_after: Duration,
        ) -> crate::error::Result<crate::coordination::RefreshRequestOutcome> {
            self.inner
                .request_refresh(requested_by, now, cooldown, stale_after)
                .await
        }
    }

    #[tokio::test]
    async fn follower_drops_its_copy_when_the_store_confirms_no_publication() {
        let vanishing = Arc::new(VanishingStore {
            inner: InMemoryStatsStore::new(),
            vanished: std::sync::atomic::AtomicBool::new(false),
        });
        let store: Arc<dyn StatsStore> = vanishing.clone();
        let state = mock_state_with_dashboard(
            MockMetadataStore::new(),
            MockStorageBackend::new(),
            DashboardState::for_tests_with_stats_store(Vec::new(), store.clone()),
        )
        .await;
        refresh_dashboard_stats(&state).await.expect("publish");
        assert_eq!(
            state.dashboard.stats().current().map(|s| s.version),
            Some(1)
        );

        // The row is gone (deleted, control reset, pre-feature restore):
        // the replica must stop serving it rather than diverge.
        vanishing
            .vanished
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(sync_from_store(&state).await.unwrap(), None);
        assert!(state.dashboard.stats().current().is_none());
        let err = crate::services::get_dashboard_stats(&state, None).unwrap_err();
        assert!(matches!(
            err,
            crate::error::GuardianError::DataUnavailable(_)
        ));
    }

    /// A failed walk hands the lease back so a healthy replica can take
    /// over on its next tick; a healthy walk keeps it.
    #[tokio::test]
    async fn leader_tick_releases_the_lease_on_a_failed_walk() {
        let store: Arc<dyn StatsStore> = Arc::new(InMemoryStatsStore::new());
        let failing = mock_state_with_dashboard(
            MockMetadataStore::new().with_list(Err("metadata store unreachable".into())),
            MockStorageBackend::new(),
            DashboardState::for_tests_with_stats_store(Vec::new(), store.clone()),
        )
        .await;
        let leader: Arc<dyn LeaderElector> =
            Arc::new(AlwaysLeader::new(DASHBOARD_STATS_LEASE, "a"));
        assert_eq!(
            leader_tick(&failing, &leader, Duration::from_secs(300)).await,
            TickOutcome::WalkFailed
        );
        assert_eq!(store.current_version().await.unwrap(), None);
        assert_eq!(
            store
                .read_control(failing.clock.now())
                .await
                .unwrap()
                .refresh_started_at,
            None,
            "failed walk leaves no in-progress marker behind"
        );

        let healthy = mock_state_with_dashboard(
            MockMetadataStore::new(),
            MockStorageBackend::new(),
            DashboardState::for_tests_with_stats_store(Vec::new(), store.clone()),
        )
        .await;
        assert_eq!(
            leader_tick(&healthy, &leader, Duration::from_secs(300)).await,
            TickOutcome::Published
        );
        assert_eq!(
            leader_tick(&healthy, &leader, Duration::from_secs(300)).await,
            TickOutcome::Idle
        );
    }
}

#[cfg(all(test, feature = "postgres"))]
mod postgres_tests {
    use super::*;
    use crate::ack::AckRegistry;
    use crate::builder::clock::test::MockClock;
    use crate::coordination::StatsStore;
    use crate::coordination::postgres::{PgLeaseElector, PgStatsStore};
    use crate::dashboard::DashboardState;
    use crate::storage::postgres::build_postgres_pool_lazy;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use crate::testing::pg::test_database_url;

    async fn replica(metadata: MockMetadataStore, store: Arc<dyn StatsStore>) -> AppState {
        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");
        AppState {
            storage: Arc::new(MockStorageBackend::new()),
            metadata: Arc::new(metadata),
            network_client: Arc::new(MockNetworkClient::new()),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::fixed("2026-09-17T12:00:00Z")),
            dashboard: Arc::new(DashboardState::for_tests_with_stats_store(
                Vec::new(),
                store,
            )),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    /// Replica A holds the lease and its walk fails (a replica-local
    /// fault). It must hand the lease back immediately so replica B can
    /// acquire and publish on its next tick, without waiting for the TTL.
    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn failed_walk_releases_the_lease_so_another_replica_publishes() {
        let url = test_database_url().await;
        let pool = build_postgres_pool_lazy(&url, 8).unwrap();
        {
            let mut conn = pool.get().await.unwrap();
            diesel_async::RunQueryDsl::execute(
                diesel::sql_query("DELETE FROM dashboard_stats_snapshots"),
                &mut conn,
            )
            .await
            .unwrap();
            diesel_async::RunQueryDsl::execute(
                diesel::sql_query(
                    "UPDATE dashboard_stats_control SET last_published_at = NULL, \
                     refresh_requested_at = NULL, refresh_requested_by = NULL, \
                     refresh_started_at = NULL, refresh_started_by = NULL, \
                     last_operator_request_at = NULL WHERE id = TRUE",
                ),
                &mut conn,
            )
            .await
            .unwrap();
        }
        let store: Arc<dyn StatsStore> = Arc::new(PgStatsStore::new(pool.clone(), None));
        let lease_name = format!("stats-failover-{}", Utc::now().timestamp_micros());
        let leader_a: Arc<dyn LeaderElector> =
            Arc::new(PgLeaseElector::new(pool.clone(), &lease_name, "replica-a"));
        let leader_b: Arc<dyn LeaderElector> =
            Arc::new(PgLeaseElector::new(pool.clone(), &lease_name, "replica-b"));
        let interval = Duration::from_secs(300);

        let replica_a = replica(
            MockMetadataStore::new().with_list(Err("replica-local fault".into())),
            store.clone(),
        )
        .await;
        let replica_b = replica(MockMetadataStore::new(), store.clone()).await;

        // A acquires first and fails its walk.
        assert_eq!(
            leader_tick(&replica_a, &leader_a, interval).await,
            TickOutcome::WalkFailed
        );
        assert_eq!(store.current_version().await.unwrap(), None);

        // B's very next tick takes the released lease and publishes.
        assert_eq!(
            leader_tick(&replica_b, &leader_b, interval).await,
            TickOutcome::Published
        );
        assert_eq!(store.current_version().await.unwrap(), Some(1));

        // While B holds the lease, A (out of backoff) is not the leader.
        assert_eq!(
            leader_tick(&replica_a, &leader_a, interval).await,
            TickOutcome::NotLeader
        );
        // A queued operator request is served by B on its next tick.
        store
            .request_refresh(
                "op",
                Utc::now(),
                STATS_REFRESH_COOLDOWN,
                STATS_REFRESH_STALE_AFTER,
            )
            .await
            .unwrap();
        assert_eq!(
            leader_tick(&replica_b, &leader_b, interval).await,
            TickOutcome::Published
        );
        assert_eq!(store.current_version().await.unwrap(), Some(2));
    }
}
