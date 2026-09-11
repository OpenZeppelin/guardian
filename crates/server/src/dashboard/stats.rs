//! Background-maintained inventory and Miden vault aggregates for
//! `GET /dashboard/stats` (issue #371).
//!
//! The cross-operator dashboard used to reconstruct "assets under
//! guard" client-side: a full walk of `GET /dashboard/accounts` plus
//! one `GET /dashboard/accounts/{id}/snapshot` per recently-updated
//! account — roughly 1,100 requests per refresh on a 2,400-account
//! Guardian, which the code-default HTTP rate limits cut off. Only the
//! server can aggregate its stored states within a bounded budget, so
//! this module does it once per refresh interval and publishes an
//! immutable [`StatsSnapshot`] that the request path reads without
//! touching storage (FR-5 / FR-6).
//!
//! Refresh algorithm (per replica, [`refresh_dashboard_stats`]):
//!
//! 1. `metadata.list()` for the authoritative account-id set (cheap,
//!    already used by `/dashboard/info`).
//! 2. Walk `metadata.list_paged` in pages of [`STATS_PAGE_SIZE`] so a
//!    large inventory is read in bounded batches (Postgres: one
//!    indexed query per page). The `(updated_at DESC, account_id)`
//!    cursor can skip a row whose `updated_at` is bumped mid-walk, so
//!    any listed id the walk missed is fetched individually afterwards.
//! 3. Batch-pull the Miden accounts' states in chunks of
//!    [`STATS_PAGE_SIZE`], then decode each vault **once per
//!    commitment**: an account whose state commitment matches the
//!    previous snapshot reuses the previous outcome (the decoded vault,
//!    or the "undecodable" verdict), so a steady-state refresh decodes
//!    only the accounts that actually changed. Decoding runs on the
//!    blocking pool so it never stalls the request-serving runtime.
//! 4. Publish the new snapshot atomically. A refresh that fails at any
//!    storage read — metadata listing, paging, or a batched state
//!    pull — keeps the previous snapshot published ("stale beats
//!    absent"), logs, and bumps
//!    `guardian_dashboard_stats_refresh_failures_total`; staleness is
//!    always visible through the response's `as_of` (FR-7). Only a
//!    state row that is genuinely absent is reported as
//!    `state_unavailable`. The loop sleeps a full interval *after* each
//!    refresh, so a walk that outruns the interval never runs
//!    back-to-back.
//!
//! Coverage semantics (FR-4): every Miden account that passes the
//! `updated_since` filter is *eligible*; it is *covered* when its vault
//! decoded, otherwise *skipped* under a stable [`SkipReason`]. EVM
//! accounts have no Miden vault and are never eligible (out of scope
//! for #371). `covered + skipped == eligible` always holds, and any
//! skip makes the aggregate `complete: false` — a missing or
//! undecodable state never masquerades as a zero balance.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use chrono::{DateTime, Utc};
use guardian_shared::FromJson;
use metrics::{counter, gauge, histogram};
use miden_protocol::asset::Asset;

use crate::builder::clock::Clock;
use crate::metadata::{AccountListCursor, AccountMetadata, MetadataStore};
use crate::metrics::names::{
    DASHBOARD_STATS_REFRESH_DURATION_SECONDS, DASHBOARD_STATS_REFRESH_FAILURES_TOTAL,
    DASHBOARD_STATS_REFRESH_TIMESTAMP_SECONDS,
};
use crate::services::normalized_authorized_signer_count;
use crate::state::AppState;
use crate::storage::StorageBackend;

/// Accounts read per metadata page and per batched state pull during
/// a refresh. Bounds the working set of one storage round trip
/// independently of inventory size (FR-6).
pub const STATS_PAGE_SIZE: u32 = 200;

/// Mutually exclusive lifecycle bucket (FR-3): `released` wins over
/// `paused`, which wins over `active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkipReason {
    /// Metadata exists but the state row could not be loaded (missing
    /// or the batched read failed).
    StateUnavailable,
    /// The stored state blob does not deserialize as a Miden `Account`.
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

/// Per-account vault totals, keyed by faucet id hex. Fungible amounts
/// are widened to `u128` at decode time so no later sum can overflow.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VaultSummary {
    pub fungible: BTreeMap<String, u128>,
    pub non_fungible: BTreeMap<String, u64>,
}

/// What the refresh learned about one account's vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultOutcome {
    /// Decoded from the state row at `AccountStatsRecord::state_commitment`.
    Decoded(VaultSummary),
    Skipped(SkipReason),
    /// EVM account: no Miden vault exists, never eligible.
    NotApplicable,
}

/// One account's contribution to the aggregate. Kept in memory between
/// refreshes (a few hundred bytes per account) so the request path can
/// apply an arbitrary `updated_since` cutoff without storage reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStatsRecord {
    pub account_id: String,
    /// Parsed metadata `updated_at` (the value `DashboardAccountSummary`
    /// exposes). `None` when the stored string is not RFC3339; such a
    /// record still counts toward totals but never matches a time
    /// window or `updated_since` filter.
    pub updated_at: Option<DateTime<Utc>>,
    pub auth_method: &'static str,
    pub authorized_signer_count: usize,
    pub lifecycle: AccountLifecycle,
    /// Commitment of the state row the vault outcome was derived from;
    /// `None` when no row could be read (or for EVM accounts). Lets the
    /// next refresh reuse `vault` — decoded or undecodable — when the
    /// stored state has not changed.
    pub state_commitment: Option<String>,
    pub vault: VaultOutcome,
}

/// Unfiltered account counts (FR-3), computed once per refresh.
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
            VaultOutcome::Skipped(reason) => {
                self.eligible += 1;
                *self.skipped.entry(*reason).or_insert(0) += 1;
            }
            VaultOutcome::Decoded(vault) => {
                self.eligible += 1;
                self.covered += 1;
                for (faucet, amount) in &vault.fungible {
                    *self.fungible.entry(faucet.clone()).or_insert(0) += amount;
                }
                for (faucet, count) in &vault.non_fungible {
                    *self.non_fungible.entry(faucet.clone()).or_insert(0) += count;
                }
            }
        }
    }
}

/// Immutable result of one refresh. `records` is sorted by
/// `updated_at` descending (unparseable timestamps last) so an
/// `updated_since` cutoff is a prefix of the slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub as_of: DateTime<Utc>,
    records: Vec<AccountStatsRecord>,
    pub accounts: AccountCounts,
    all_assets: AssetAggregate,
}

impl StatsSnapshot {
    pub fn new(as_of: DateTime<Utc>, mut records: Vec<AccountStatsRecord>) -> Self {
        records.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.account_id.cmp(&b.account_id))
        });
        let accounts = Self::count_accounts(as_of, &records);
        let all_assets = aggregate_assets(&records);
        Self {
            as_of,
            records,
            accounts,
            all_assets,
        }
    }

    #[cfg(test)]
    pub fn records(&self) -> &[AccountStatsRecord] {
        &self.records
    }

    /// Asset totals over accounts whose metadata `updated_at >= since`
    /// (FR-2); `None` aggregates every account and is served from the
    /// value precomputed at refresh time.
    pub fn assets_since(&self, since: Option<DateTime<Utc>>) -> AssetAggregate {
        match since {
            None => self.all_assets.clone(),
            Some(since) => {
                let end = self
                    .records
                    .partition_point(|r| r.updated_at.is_some_and(|ts| ts >= since));
                aggregate_assets(&self.records[..end])
            }
        }
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
                .entry(record.auth_method.to_string())
                .or_insert(0) += 1;
            *counts
                .by_auth_method_and_signer_count
                .entry((
                    record.auth_method.to_string(),
                    record.authorized_signer_count,
                ))
                .or_insert(0) += 1;
            if let Some(updated_at) = record.updated_at {
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

fn aggregate_assets(records: &[AccountStatsRecord]) -> AssetAggregate {
    let mut aggregate = AssetAggregate::default();
    for record in records {
        aggregate.add(record);
    }
    aggregate
}

/// Process-local holder of the latest published snapshot. Reads are a
/// single `Arc` clone under a short read lock; the refresher is the
/// only writer.
#[derive(Default)]
pub struct DashboardStatsCache {
    current: RwLock<Option<Arc<StatsSnapshot>>>,
}

impl std::fmt::Debug for DashboardStatsCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let as_of = self.current().map(|s| s.as_of);
        f.debug_struct("DashboardStatsCache")
            .field("as_of", &as_of)
            .finish()
    }
}

impl DashboardStatsCache {
    /// Latest snapshot, or `None` until the first refresh completes.
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
}

/// Run one refresh against `state` and publish the result. Returns the
/// published snapshot; on error the previous snapshot (if any) stays
/// published.
pub async fn refresh_dashboard_stats(state: &AppState) -> Result<Arc<StatsSnapshot>, String> {
    let cache = state.dashboard.stats();
    let previous = cache.current();
    let started = Instant::now();
    let result = build_snapshot(
        &state.storage,
        &state.metadata,
        &state.clock,
        previous.as_deref(),
    )
    .await;
    histogram!(DASHBOARD_STATS_REFRESH_DURATION_SECONDS).record(started.elapsed().as_secs_f64());
    match result {
        Ok(snapshot) => {
            let snapshot = Arc::new(snapshot);
            cache.publish(snapshot.clone());
            gauge!(DASHBOARD_STATS_REFRESH_TIMESTAMP_SECONDS)
                .set(snapshot.as_of.timestamp() as f64);
            tracing::debug!(
                target: "dashboard.stats",
                accounts = snapshot.accounts.total,
                eligible = snapshot.all_assets.eligible,
                covered = snapshot.all_assets.covered,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "dashboard stats refreshed"
            );
            Ok(snapshot)
        }
        Err(error) => {
            counter!(DASHBOARD_STATS_REFRESH_FAILURES_TOTAL).increment(1);
            Err(error)
        }
    }
}

/// Spawn the refresh loop alongside the other background jobs. The
/// first refresh runs immediately so the endpoint becomes available as
/// soon as the initial walk completes; every later one starts a full
/// interval *after* the previous finished, so a slow walk can never
/// pin a core by running back-to-back (FR-6).
pub fn start_stats_refresher(state: AppState) {
    tokio::spawn(run_stats_refresher(state));
}

async fn run_stats_refresher(state: AppState) {
    let interval = state.dashboard.stats_refresh_interval();
    loop {
        if let Err(error) = refresh_dashboard_stats(&state).await {
            tracing::warn!(
                target: "dashboard.stats",
                %error,
                "dashboard stats refresh failed; previous snapshot left published"
            );
        }
        tokio::time::sleep(interval).await;
    }
}

/// Assemble a snapshot from storage. Pure with respect to the cache so
/// tests can drive it directly; `previous` supplies vault outcomes to
/// reuse for unchanged commitments. Any storage-read failure is an
/// `Err` so the caller keeps the previous snapshot.
pub async fn build_snapshot(
    storage: &Arc<dyn StorageBackend>,
    metadata: &Arc<dyn MetadataStore>,
    clock: &Arc<dyn Clock>,
    previous: Option<&StatsSnapshot>,
) -> Result<StatsSnapshot, String> {
    let as_of = clock.now();
    let listed_ids = metadata
        .list()
        .await
        .map_err(|e| format!("list account metadata: {e}"))?;

    let mut metadatas: HashMap<String, AccountMetadata> = HashMap::with_capacity(listed_ids.len());
    let mut cursor: Option<AccountListCursor> = None;
    loop {
        let page = metadata
            .list_paged(STATS_PAGE_SIZE, cursor.take(), None)
            .await
            .map_err(|e| format!("page account metadata: {e}"))?;
        let page_len = page.len();
        let last = page
            .last()
            .map(|m| (m.updated_at.clone(), m.account_id.clone()));
        for m in page {
            metadatas.insert(m.account_id.clone(), m);
        }
        if page_len < STATS_PAGE_SIZE as usize {
            break;
        }
        match last.and_then(|(updated_at, account_id)| {
            parse_rfc3339_utc(&updated_at).map(|last_updated_at| AccountListCursor {
                last_updated_at,
                last_account_id: account_id,
            })
        }) {
            Some(next) => cursor = Some(next),
            None => {
                // Cannot continue the cursor walk from an unparseable
                // timestamp; the id-recovery pass below fills the gap.
                tracing::warn!(
                    target: "dashboard.stats",
                    "dashboard stats: account list cursor not RFC3339; falling back to point reads"
                );
                break;
            }
        }
        tokio::task::yield_now().await;
    }

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

    // Outcomes worth carrying over when the state row is unchanged: a
    // decoded vault, or the verdict that the blob does not decode (so a
    // permanently corrupt row is decoded — and warned about — once,
    // not once per interval).
    let reusable: HashMap<&str, (&str, &VaultOutcome)> = previous
        .map(|p| {
            p.records
                .iter()
                .filter_map(|r| {
                    let commitment = r.state_commitment.as_deref()?;
                    match &r.vault {
                        VaultOutcome::Decoded(_)
                        | VaultOutcome::Skipped(SkipReason::StateUndecodable) => {
                            Some((r.account_id.as_str(), (commitment, &r.vault)))
                        }
                        _ => None,
                    }
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

    // Per Miden account: the commitment of the row we read (if any) and
    // the vault outcome. A missing row is `state_unavailable`; a failed
    // batched read aborts the refresh instead of publishing a snapshot
    // that under-reports assets for a whole interval.
    let mut commitments: HashMap<String, String> = HashMap::with_capacity(miden_ids.len());
    let mut vault_outcomes: HashMap<String, VaultOutcome> = HashMap::with_capacity(miden_ids.len());
    for chunk in miden_ids.chunks(STATS_PAGE_SIZE as usize) {
        let refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
        let mut states = storage
            .pull_states_batch(&refs)
            .await
            .map_err(|e| format!("batch-pull account states: {e}"))?;
        let mut to_decode = Vec::new();
        for id in chunk {
            match states.remove(id) {
                None => {
                    vault_outcomes.insert(
                        id.clone(),
                        VaultOutcome::Skipped(SkipReason::StateUnavailable),
                    );
                }
                Some(state) => {
                    match reusable.get(id.as_str()) {
                        Some((commitment, outcome)) if *commitment == state.commitment => {
                            vault_outcomes.insert(id.clone(), (*outcome).clone());
                        }
                        _ => to_decode.push((id.clone(), state.state_json)),
                    }
                    commitments.insert(id.clone(), state.commitment);
                }
            }
        }
        if to_decode.is_empty() {
            continue;
        }
        let decoded = tokio::task::spawn_blocking(move || {
            to_decode
                .into_iter()
                .map(|(id, state_json)| {
                    let outcome = match decode_vault(&state_json) {
                        Ok(vault) => VaultOutcome::Decoded(vault),
                        Err(error) => {
                            tracing::warn!(
                                target: "dashboard.stats",
                                account_id = %id,
                                %error,
                                "dashboard stats: stored Miden account did not decode"
                            );
                            VaultOutcome::Skipped(SkipReason::StateUndecodable)
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

    let records = metadatas
        .into_iter()
        .map(|(id, m)| {
            let vault = if m.network_config.is_evm() {
                VaultOutcome::NotApplicable
            } else {
                vault_outcomes
                    .remove(&id)
                    .unwrap_or(VaultOutcome::Skipped(SkipReason::StateUnavailable))
            };
            AccountStatsRecord {
                updated_at: parse_rfc3339_utc(&m.updated_at),
                auth_method: m.auth.method_label(),
                authorized_signer_count: normalized_authorized_signer_count(&m.auth),
                lifecycle: AccountLifecycle::of(&m),
                state_commitment: commitments.remove(&id),
                account_id: id,
                vault,
            }
        })
        .collect();

    Ok(StatsSnapshot::new(as_of, records))
}

/// Decode the vault totals of a stored Miden account blob. Mirrors the
/// per-account snapshot decode so both surfaces agree on faucet ids.
pub fn decode_vault(state_json: &serde_json::Value) -> Result<VaultSummary, String> {
    let account = miden_protocol::account::Account::from_json(state_json)?;
    let mut summary = VaultSummary::default();
    for asset in account.vault().assets() {
        match asset {
            Asset::Fungible(a) => {
                *summary.fungible.entry(a.faucet_id().to_hex()).or_insert(0) +=
                    u128::from(u64::from(a.amount()));
            }
            Asset::NonFungible(a) => {
                *summary
                    .non_fungible
                    .entry(a.faucet_id().to_hex())
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

    /// Deterministic fungible-faucet id from a seed byte.
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
    use crate::metadata::auth::Auth;
    use crate::metadata::{AccountMetadata, NetworkConfig};
    use crate::state_object::StateObject;
    use crate::testing::helpers::create_test_app_state;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use std::time::Duration;

    fn ts(value: &str) -> DateTime<Utc> {
        parse_rfc3339_utc(value).expect("test timestamp")
    }

    fn record(
        id: &str,
        updated_at: Option<&str>,
        auth_method: &'static str,
        signers: usize,
        lifecycle: AccountLifecycle,
        vault: VaultOutcome,
    ) -> AccountStatsRecord {
        AccountStatsRecord {
            account_id: id.to_string(),
            updated_at: updated_at.map(ts),
            auth_method,
            authorized_signer_count: signers,
            lifecycle,
            state_commitment: None,
            vault,
        }
    }

    fn decoded(fungible: &[(&str, u128)], non_fungible: &[(&str, u64)]) -> VaultOutcome {
        VaultOutcome::Decoded(VaultSummary {
            fungible: fungible.iter().map(|(f, a)| (f.to_string(), *a)).collect(),
            non_fungible: non_fungible
                .iter()
                .map(|(f, c)| (f.to_string(), *c))
                .collect(),
        })
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
            clock: Arc::new(MockClock::fixed("2026-09-11T12:00:00Z")),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

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
        ];
        let snapshot = StatsSnapshot::new(as_of, records);
        let counts = &snapshot.accounts;
        assert_eq!(counts.total, 5);
        assert_eq!((counts.active, counts.paused, counts.released), (3, 1, 1));
        assert_eq!(counts.by_auth_method["miden_falcon"], 3);
        assert_eq!(counts.by_auth_method["miden_ecdsa"], 1);
        assert_eq!(counts.by_auth_method["evm"], 1);
        assert_eq!(
            counts.by_auth_method_and_signer_count[&("miden_falcon".to_string(), 1)],
            2
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
        // Sorted newest-first with unparseable timestamps last.
        let order: Vec<&str> = snapshot
            .records()
            .iter()
            .map(|r| r.account_id.as_str())
            .collect();
        assert_eq!(order, vec!["a", "c", "b", "d", "e"]);
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
        let snapshot = StatsSnapshot::new(as_of, records);

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

        // Account counts are unfiltered regardless of the cutoff.
        assert_eq!(snapshot.accounts.total, 4);
    }

    #[test]
    fn fungible_totals_widen_beyond_u64() {
        let max = u128::from(u64::MAX);
        let records = vec![
            record(
                "a",
                Some("2026-09-10T00:00:00Z"),
                "miden_falcon",
                1,
                AccountLifecycle::Active,
                decoded(&[("0xf1", max)], &[]),
            ),
            record(
                "b",
                Some("2026-09-10T00:00:00Z"),
                "miden_falcon",
                1,
                AccountLifecycle::Active,
                decoded(&[("0xf1", max)], &[]),
            ),
        ];
        let snapshot = StatsSnapshot::new(ts("2026-09-11T00:00:00Z"), records);
        let all = snapshot.assets_since(None);
        assert_eq!(all.fungible["0xf1"], max * 2);
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
                VaultOutcome::Skipped(SkipReason::StateUnavailable),
            ),
            record(
                "garbage",
                Some("2026-09-10T00:00:00Z"),
                "miden_ecdsa",
                1,
                AccountLifecycle::Active,
                VaultOutcome::Skipped(SkipReason::StateUndecodable),
            ),
            record(
                "missing2",
                Some("2026-09-10T00:00:00Z"),
                "miden_falcon",
                1,
                AccountLifecycle::Active,
                VaultOutcome::Skipped(SkipReason::StateUnavailable),
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
        let snapshot = StatsSnapshot::new(ts("2026-09-11T00:00:00Z"), records);
        let all = snapshot.assets_since(None);
        // EVM accounts are never eligible; skipped ones are eligible but not covered.
        assert_eq!(all.eligible, 4);
        assert_eq!(all.covered, 1);
        assert_eq!(all.skipped[&SkipReason::StateUnavailable], 2);
        assert_eq!(all.skipped[&SkipReason::StateUndecodable], 1);
        assert_eq!(all.covered + all.skipped_total(), all.eligible);
        assert!(!all.complete());
        // Skipped accounts contribute nothing — no phantom zero entries.
        assert_eq!(all.fungible.len(), 1);
        assert_eq!(all.fungible["0xf1"], 100);
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

    #[tokio::test]
    async fn build_snapshot_walks_filesystem_backends_and_classifies_every_account() {
        let state = create_test_app_state().await;
        let f1 = faucet(0x11);
        let f2 = faucet(0x22);
        let recent = "2026-09-10T00:00:00Z";
        let stale = "2026-01-01T00:00:00Z";

        // Active Miden account with assets, updated recently.
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
        // Paused Miden account with assets, updated long ago.
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
        // Released Miden account whose state blob is garbage.
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
        // Miden account with metadata but no state row at all.
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
        // EVM account: never eligible.
        state
            .metadata
            .set(metadata(
                "acc-e",
                Auth::EvmEcdsa {
                    signers: vec!["0xe1".to_string(), "0xe2".to_string(), "0xe3".to_string()],
                },
                evm_network(),
                recent,
            ))
            .await
            .unwrap();

        let snapshot = build_snapshot(&state.storage, &state.metadata, &state.clock, None)
            .await
            .expect("snapshot");

        let counts = &snapshot.accounts;
        assert_eq!(counts.total, 5);
        assert_eq!((counts.active, counts.paused, counts.released), (3, 1, 1));
        assert_eq!(counts.by_auth_method["miden_falcon"], 3);
        assert_eq!(counts.by_auth_method["miden_ecdsa"], 1);
        assert_eq!(counts.by_auth_method["evm"], 1);
        assert_eq!(
            counts.by_auth_method_and_signer_count[&("miden_falcon".to_string(), 2)],
            1
        );
        assert_eq!(
            counts.by_auth_method_and_signer_count[&("evm".to_string(), 3)],
            1
        );

        let by_id: HashMap<&str, &AccountStatsRecord> = snapshot
            .records()
            .iter()
            .map(|r| (r.account_id.as_str(), r))
            .collect();
        assert!(matches!(by_id["acc-a"].vault, VaultOutcome::Decoded(_)));
        assert_eq!(
            by_id["acc-a"].state_commitment.as_deref(),
            Some(a.commitment.as_str())
        );
        assert!(matches!(by_id["acc-b"].vault, VaultOutcome::Decoded(_)));
        assert_eq!(
            by_id["acc-c"].vault,
            VaultOutcome::Skipped(SkipReason::StateUndecodable)
        );
        // The undecodable row's commitment is still recorded so the
        // verdict can be reused next refresh.
        assert_eq!(by_id["acc-c"].state_commitment.as_deref(), Some("0xc"));
        assert_eq!(
            by_id["acc-d"].vault,
            VaultOutcome::Skipped(SkipReason::StateUnavailable)
        );
        assert_eq!(by_id["acc-d"].state_commitment, None);
        assert_eq!(by_id["acc-e"].vault, VaultOutcome::NotApplicable);
        assert_eq!(by_id["acc-e"].state_commitment, None);

        let all = snapshot.assets_since(None);
        assert_eq!((all.eligible, all.covered), (4, 2));
        assert_eq!(all.skipped[&SkipReason::StateUndecodable], 1);
        assert_eq!(all.skipped[&SkipReason::StateUnavailable], 1);
        assert!(!all.complete());
        assert_eq!(all.fungible[&f1.to_hex()], 1_500);
        assert_eq!(all.fungible[&f2.to_hex()], 7);

        // Only the recently-updated accounts are eligible under a cutoff;
        // the stale paused account drops out of both eligibility and totals.
        let since = snapshot.assets_since(Some(ts("2026-09-01T00:00:00Z")));
        assert_eq!((since.eligible, since.covered), (3, 1));
        assert_eq!(since.fungible[&f1.to_hex()], 1_000);
    }

    #[tokio::test]
    async fn build_snapshot_reuses_previous_outcome_only_when_commitment_is_unchanged() {
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
        // A second account whose stored blob is perfectly decodable but
        // whose previous verdict (under the same commitment) was
        // "undecodable": if that verdict is reused, no decode happened.
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
            VaultOutcome::Skipped(SkipReason::StateUndecodable),
        );
        prev_g.state_commitment = Some("0xsame".to_string());
        let previous = StatsSnapshot::new(ts("2026-09-11T00:00:00Z"), vec![prev_a, prev_g]);

        let reused = build_snapshot(
            &state.storage,
            &state.metadata,
            &state.clock,
            Some(&previous),
        )
        .await
        .unwrap();
        let by_id: HashMap<&str, &AccountStatsRecord> = reused
            .records()
            .iter()
            .map(|r| (r.account_id.as_str(), r))
            .collect();
        assert_eq!(
            by_id["acc-a"].vault, sentinel,
            "unchanged commitment reuses the decoded vault"
        );
        assert_eq!(
            by_id["acc-g"].vault,
            VaultOutcome::Skipped(SkipReason::StateUndecodable),
            "unchanged commitment reuses the undecodable verdict without re-decoding"
        );

        // A different previous commitment forces a fresh decode of both.
        let mut stale_a = record(
            "acc-a",
            Some("2026-09-10T00:00:00Z"),
            "miden_falcon",
            1,
            AccountLifecycle::Active,
            sentinel,
        );
        stale_a.state_commitment = Some("0xolder".to_string());
        let mut stale_g = record(
            "acc-g",
            Some("2026-09-10T00:00:00Z"),
            "miden_falcon",
            1,
            AccountLifecycle::Active,
            VaultOutcome::Skipped(SkipReason::StateUndecodable),
        );
        stale_g.state_commitment = Some("0xolder".to_string());
        let stale_previous = StatsSnapshot::new(ts("2026-09-11T00:00:00Z"), vec![stale_a, stale_g]);
        let redecoded = build_snapshot(
            &state.storage,
            &state.metadata,
            &state.clock,
            Some(&stale_previous),
        )
        .await
        .unwrap();
        let by_id: HashMap<&str, &AccountStatsRecord> = redecoded
            .records()
            .iter()
            .map(|r| (r.account_id.as_str(), r))
            .collect();
        match &by_id["acc-a"].vault {
            VaultOutcome::Decoded(vault) => {
                assert_eq!(vault.fungible[&f1.to_hex()], 1_000);
                assert!(!vault.fungible.contains_key("0xsentinel"));
            }
            other => panic!("expected decoded vault, got {other:?}"),
        }
        assert_eq!(
            by_id["acc-a"].state_commitment.as_deref(),
            Some(a.commitment.as_str())
        );
        assert!(matches!(by_id["acc-g"].vault, VaultOutcome::Decoded(_)));
    }

    #[tokio::test]
    async fn build_snapshot_recovers_accounts_the_cursor_walk_missed() {
        // `list()` knows two accounts; the paged walk only surfaces one
        // (a row bumped ahead of the cursor mid-walk). The missing id is
        // fetched individually so the snapshot stays complete.
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

        let snapshot = build_snapshot(&state.storage, &state.metadata, &state.clock, None)
            .await
            .unwrap();
        let ids: Vec<&str> = snapshot
            .records()
            .iter()
            .map(|r| r.account_id.as_str())
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
        assert_eq!(snapshot.accounts.total, 2);
        assert_eq!(snapshot.assets_since(None).eligible, 0);
    }

    #[tokio::test]
    async fn build_snapshot_marks_missing_state_rows_unavailable() {
        let meta = metadata(
            "a",
            falcon(&["0x1"]),
            NetworkConfig::miden_default(),
            "2026-09-10T00:00:00Z",
        );
        let metadata_store = MockMetadataStore::new()
            .with_list(Ok(vec!["a".to_string()]))
            .with_list_paged(Ok(vec![meta]));
        // The default `pull_states_batch` treats a per-account
        // `pull_state` failure as "row absent" (the mock answers with
        // an error here), which is reported as `state_unavailable`.
        let storage = MockStorageBackend::new().with_pull_state(Err("db down".to_string()));
        let state = mock_state(metadata_store, storage).await;

        let snapshot = build_snapshot(&state.storage, &state.metadata, &state.clock, None)
            .await
            .unwrap();
        let all = snapshot.assets_since(None);
        assert_eq!((all.eligible, all.covered), (1, 0));
        assert_eq!(all.skipped[&SkipReason::StateUnavailable], 1);
        assert!(!all.complete());
    }

    #[tokio::test]
    async fn refresh_failure_keeps_previous_snapshot_published() {
        let metadata_store = MockMetadataStore::new()
            // Responses pop LIFO: first refresh succeeds (empty
            // inventory), second fails at `list()`.
            .with_list(Err("metadata store unreachable".to_string()))
            .with_list(Ok(vec![]));
        let state = mock_state(metadata_store, MockStorageBackend::new()).await;

        let first = refresh_dashboard_stats(&state)
            .await
            .expect("first refresh");
        assert!(Arc::ptr_eq(
            &first,
            &state.dashboard.stats().current().unwrap()
        ));

        let err = refresh_dashboard_stats(&state).await.unwrap_err();
        assert!(err.contains("metadata store unreachable"));
        assert!(Arc::ptr_eq(
            &first,
            &state.dashboard.stats().current().unwrap()
        ));
    }

    /// The loop publishes immediately, then once per configured
    /// interval (60s in the test config). Uses paused tokio time; each
    /// tick publishes a fresh `Arc`, which is the observable.
    #[tokio::test(start_paused = true)]
    async fn refresher_publishes_immediately_then_per_interval() {
        let state = mock_state(MockMetadataStore::new(), MockStorageBackend::new()).await;
        let interval = state.dashboard.stats_refresh_interval();
        assert_eq!(interval, Duration::from_secs(60));
        let task = tokio::spawn(run_stats_refresher(state.clone()));

        async fn wait_for_new(
            state: &AppState,
            previous: Option<&Arc<StatsSnapshot>>,
        ) -> Arc<StatsSnapshot> {
            for _ in 0..200 {
                if let Some(current) = state.dashboard.stats().current()
                    && previous.is_none_or(|p| !Arc::ptr_eq(p, &current))
                {
                    return current;
                }
                tokio::task::yield_now().await;
            }
            panic!("refresher did not publish a new snapshot");
        }

        let first = wait_for_new(&state, None).await;
        tokio::time::advance(interval).await;
        let second = wait_for_new(&state, Some(&first)).await;
        tokio::time::advance(interval * 2).await;
        wait_for_new(&state, Some(&second)).await;
        task.abort();
    }
}
