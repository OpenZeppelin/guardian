use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::state_object::StateObject;

/// Stable lifecycle status identifiers used in the typed `status_kind`
/// column promoted by the Phase A migration. Service-layer callers
/// pass these to filter and group cross-account aggregates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaStatusKind {
    Pending,
    Candidate,
    Canonical,
    Discarded,
}

impl DeltaStatusKind {
    /// Stable lower-snake-case name for the Postgres `status_kind`
    /// column and the dashboard status-filter wire shape.
    pub fn as_str(&self) -> &'static str {
        match self {
            DeltaStatusKind::Pending => "pending",
            DeltaStatusKind::Candidate => "candidate",
            DeltaStatusKind::Canonical => "canonical",
            DeltaStatusKind::Discarded => "discarded",
        }
    }
}

/// Cursor parameters for the per-account delta history read. Sort key
/// is `nonce DESC` against the `(account_id, nonce)` UNIQUE constraint,
/// so cursor stability is fully guaranteed under both concurrent
/// inserts and status updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDeltaCursor {
    pub last_nonce: i64,
}

/// Cursor parameters for the per-account proposal history read.
/// `(account_id, nonce)` is NOT unique on `delta_proposals` —
/// concurrent operators can submit two proposals for the same nonce
/// — so the cursor includes `last_commitment` as the deterministic
/// tiebreaker (`(account_id, commitment)` is the UNIQUE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountProposalCursor {
    pub last_nonce: i64,
    pub last_commitment: String,
}

/// Cursor parameters for the global delta feed. Sort key is
/// `(status_timestamp DESC, account_id ASC, nonce ASC)`. Status
/// timestamp is mutable on deltas (a candidate transitioning to
/// canonical bumps it), so cursor stability has the FR-005 caveat
/// for this feed only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalDeltaCursor {
    pub last_status_timestamp: DateTime<Utc>,
    pub last_account_id: String,
    pub last_nonce: i64,
}

/// Cursor parameters for the global proposal feed. Sort key is
/// `(status_timestamp DESC, account_id ASC, nonce ASC, commitment ASC)`.
/// Originating timestamp is immutable while a proposal remains in the
/// queue. The `commitment` tiebreaker handles the case where two
/// in-flight proposals share a nonce within the same account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalProposalCursor {
    pub last_originating_timestamp: DateTime<Utc>,
    pub last_account_id: String,
    pub last_nonce: i64,
    pub last_commitment: String,
}

/// Aggregate counts of persisted deltas grouped by lifecycle status.
/// Returned by [`StorageBackend::count_deltas_by_status`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeltaStatusCounts {
    pub candidate: u64,
    pub canonical: u64,
    pub discarded: u64,
}

/// One row returned by the global delta feed. Carries the parent
/// `account_id` so the dashboard can group/link without an extra
/// lookup.
#[derive(Debug, Clone)]
pub struct GlobalDeltaRow {
    pub account_id: String,
    pub delta: DeltaObject,
}

/// One row returned by a per-account or global proposal feed. Carries
/// the storage-layer `commitment` (the cryptographic identifier
/// cosigners are signing — `delta_proposals.commitment`), which the
/// embedded [`DeltaObject`] does not preserve.
#[derive(Debug, Clone)]
pub struct ProposalRecord {
    pub account_id: String,
    pub commitment: String,
    pub proposal: DeltaObject,
}
pub mod filesystem;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub use postgres::run_migrations;

/// Storage backend type with configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum StorageType {
    #[default]
    Filesystem,
    Postgres,
}

impl std::fmt::Display for StorageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageType::Filesystem => write!(f, "Filesystem"),
            StorageType::Postgres => write!(f, "Postgres"),
        }
    }
}

/// Storage backend trait for managing account states and deltas
#[async_trait]
pub trait StorageBackend: Send + Sync {
    /// Identifies which concrete backend is in use. The dashboard
    /// service layer reads this to decide whether the
    /// `filesystem_aggregate_threshold` (FR-029) applies — Postgres
    /// serves cross-account aggregates over indexed columns and is
    /// not bounded by the threshold.
    fn kind(&self) -> StorageType;

    async fn submit_state(&self, state: &StateObject) -> Result<(), String>;
    async fn submit_delta(&self, delta: &DeltaObject) -> Result<(), String>;
    async fn pull_state(&self, account_id: &str) -> Result<StateObject, String>;

    /// Batch fetch states for `account_ids` in a single round trip
    /// (Postgres: one `SELECT ... WHERE account_id = ANY($1)`;
    /// filesystem: bounded-concurrency parallel reads). Missing
    /// accounts are simply absent from the returned map — callers
    /// must distinguish "no state yet" from "metadata-without-state"
    /// at the service layer if needed. Used by the dashboard account
    /// list to avoid the N+1 pattern that the per-account history
    /// endpoints already collapsed.
    async fn pull_states_batch(
        &self,
        account_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, StateObject>, String> {
        // Default: sequential single-account fetches. Concrete
        // backends override with their batched form.
        //
        // Error policy: `pull_state` currently returns `Result<_,
        // String>` so we can't structurally distinguish "missing
        // state row" from "transient storage failure". Until that
        // surface is typed, we surface ANY error from `pull_state`
        // via tracing so operators see degraded reads in logs
        // instead of silent flips to `state_status: Unavailable` at
        // the dashboard layer. Concrete backends SHOULD override
        // this method with their own batched form that can
        // distinguish the two cases (postgres already does — see
        // `PostgresService::pull_states_batch`).
        let mut out = std::collections::HashMap::with_capacity(account_ids.len());
        for id in account_ids {
            match self.pull_state(id).await {
                Ok(state) => {
                    out.insert((*id).to_string(), state);
                }
                Err(e) => {
                    tracing::warn!(
                        account_id = %id,
                        error = %e,
                        "pull_states_batch: pull_state failed; treating as missing-state at dashboard layer",
                    );
                }
            }
        }
        Ok(out)
    }
    async fn pull_delta(&self, account_id: &str, nonce: u64) -> Result<DeltaObject, String>;
    async fn pull_deltas_after(
        &self,
        account_id: &str,
        from_nonce: u64,
    ) -> Result<Vec<DeltaObject>, String>;
    async fn has_pending_candidate(&self, account_id: &str) -> Result<bool, String> {
        let deltas = self.pull_deltas_after(account_id, 0).await?;
        Ok(deltas.iter().any(|delta| delta.status.is_candidate()))
    }
    async fn pull_canonical_deltas_after(
        &self,
        account_id: &str,
        from_nonce: u64,
    ) -> Result<Vec<DeltaObject>, String> {
        let deltas = self.pull_deltas_after(account_id, from_nonce).await?;
        Ok(deltas
            .into_iter()
            .filter(|delta| delta.status.is_canonical())
            .collect())
    }
    async fn submit_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String>;
    async fn pull_delta_proposal(
        &self,
        account_id: &str,
        commitment: &str,
    ) -> Result<DeltaObject, String>;
    async fn pull_all_delta_proposals(&self, account_id: &str) -> Result<Vec<DeltaObject>, String>;
    async fn pull_pending_proposals(&self, account_id: &str) -> Result<Vec<DeltaObject>, String> {
        let mut proposals = self.pull_all_delta_proposals(account_id).await?;
        proposals.retain(|proposal| proposal.status.is_pending());
        proposals.sort_by_key(|proposal| proposal.nonce);
        Ok(proposals)
    }
    async fn update_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String>;
    async fn delete_delta_proposal(&self, account_id: &str, commitment: &str)
    -> Result<(), String>;
    async fn delete_delta(&self, account_id: &str, nonce: u64) -> Result<(), String>;
    async fn update_delta_status(
        &self,
        account_id: &str,
        nonce: u64,
        status: DeltaStatus,
    ) -> Result<(), String>;

    // ----------------------------------------------------------------------
    // Dashboard read APIs — feature `005-operator-dashboard-metrics`,
    // Decision 1 (revised). These methods exist so the dashboard can
    // push pagination, sorting, and filtering down to the storage
    // layer instead of fan-out + in-memory aggregation at the service
    // layer. Postgres implements them with SQL on the indexed
    // `status_kind`/`status_timestamp` columns. Filesystem implements
    // them with the existing fan-out pattern, bounded by the
    // configured aggregate threshold (FR-029).
    // ----------------------------------------------------------------------

    /// Per-account delta history paginated newest-first by `nonce
    /// DESC`. Surfaces only deltas in the lifecycle statuses persisted
    /// in the `deltas` table — `pending` entries live in
    /// `delta_proposals` and are returned by
    /// [`Self::list_account_proposals_paged`].
    async fn list_account_deltas_paged(
        &self,
        account_id: &str,
        limit: u32,
        cursor: Option<AccountDeltaCursor>,
    ) -> Result<Vec<DeltaObject>, String>;

    /// Per-account in-flight proposal queue paginated newest-first by
    /// `(nonce DESC, commitment DESC)`. Returns only `Pending` rows
    /// from the `delta_proposals` table. Each result carries the
    /// storage-layer `commitment` (the value cosigners signed) so the
    /// service layer can pass it through to the wire shape verbatim.
    async fn list_account_proposals_paged(
        &self,
        account_id: &str,
        limit: u32,
        cursor: Option<AccountProposalCursor>,
    ) -> Result<Vec<ProposalRecord>, String>;

    /// Global delta feed across all configured accounts. Sorted by
    /// `(status_timestamp DESC, account_id ASC, nonce ASC)`. Optional
    /// `status_filter` restricts the result to one or more lifecycle
    /// kinds (`candidate`, `canonical`, `discarded`). Filesystem
    /// implementations refuse with an error when above the configured
    /// aggregate threshold (FR-029); callers map the error to
    /// `503 DataUnavailable` at the service layer.
    async fn list_global_deltas_paged(
        &self,
        limit: u32,
        cursor: Option<GlobalDeltaCursor>,
        status_filter: Option<Vec<DeltaStatusKind>>,
    ) -> Result<Vec<GlobalDeltaRow>, String>;

    /// Global in-flight proposal feed across all configured accounts.
    /// Sorted by `(originating_timestamp DESC, account_id ASC, nonce
    /// ASC, commitment ASC)`. Returns only `Pending` proposals. Each
    /// result carries the storage-layer `commitment` so the service
    /// layer can pass it through to the wire shape verbatim.
    async fn list_global_proposals_paged(
        &self,
        limit: u32,
        cursor: Option<GlobalProposalCursor>,
    ) -> Result<Vec<ProposalRecord>, String>;

    /// Aggregate count of persisted deltas grouped by lifecycle
    /// status, used by `GET /dashboard/info`. Excludes `pending`
    /// (those are counted by
    /// [`Self::count_in_flight_proposals`]).
    async fn count_deltas_by_status(&self) -> Result<DeltaStatusCounts, String>;

    /// Count of `Pending` rows in `delta_proposals` across all
    /// accounts.
    async fn count_in_flight_proposals(&self) -> Result<u64, String>;

    /// Greater of the most recent `status_timestamp` across all
    /// deltas and the most recent originating timestamp across all
    /// in-flight proposals. `None` when the inventory has produced no
    /// activity.
    async fn latest_activity_timestamp(&self) -> Result<Option<DateTime<Utc>>, String>;
}
