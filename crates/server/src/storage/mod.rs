use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::state_object::StateObject;

/// Returns `true` when a backend-formatted error string represents a
/// "row not present" outcome. Both Postgres (Diesel) and the filesystem
/// backend surface errors as `String`, so callers that need to branch
/// between "no row" and "real failure" share this heuristic instead of
/// reimplementing it. Replace with a typed error once `StorageBackend`
/// stops returning `Result<_, String>`.
pub fn is_storage_not_found(err: &str) -> bool {
    let lower = err.to_lowercase();
    lower.contains("not found") || lower.contains("notfound") || lower.contains("no such file")
}

/// Stable lifecycle status identifiers used in the typed `status_kind`
/// column promoted by the Phase A migration. Service-layer callers
/// pass these to filter and group cross-account aggregates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaStatusKind {
    Pending,
    Candidate,
    Canonical,
    Retained,
    Discarded,
}

impl DeltaStatusKind {
    /// The typed kind of a full [`DeltaStatus`], the single mapping the
    /// backends and the dashboard wire layer share.
    pub fn of(status: &DeltaStatus) -> Self {
        match status {
            DeltaStatus::Pending { .. } => DeltaStatusKind::Pending,
            DeltaStatus::Candidate { .. } => DeltaStatusKind::Candidate,
            DeltaStatus::Canonical { .. } => DeltaStatusKind::Canonical,
            DeltaStatus::Retained { .. } => DeltaStatusKind::Retained,
            DeltaStatus::Discarded { .. } => DeltaStatusKind::Discarded,
        }
    }

    /// Stable lower-snake-case name for the Postgres `status_kind`
    /// column and the dashboard status-filter wire shape.
    pub fn as_str(&self) -> &'static str {
        match self {
            DeltaStatusKind::Pending => "pending",
            DeltaStatusKind::Candidate => "candidate",
            DeltaStatusKind::Canonical => "canonical",
            DeltaStatusKind::Retained => "retained",
            DeltaStatusKind::Discarded => "discarded",
        }
    }
}

/// Whether a delta is in scope for the background reconcile pass
/// (issue #345): every `retained` row, plus client-abandoned discards
/// recent enough (status timestamp at or after `abandoned_since`) to
/// still be worth re-checking against the chain. An abandoned row with
/// an unparseable timestamp is excluded — abandoned recovery is a
/// best-effort net over rows that are kept as history either way.
pub(crate) fn is_recoverable(status: &DeltaStatus, abandoned_since: DateTime<Utc>) -> bool {
    if status.is_retained() {
        return true;
    }
    status.is_client_abandoned()
        && DateTime::parse_from_rfc3339(status.timestamp())
            .map(|at| at.with_timezone(&Utc) >= abandoned_since)
            .unwrap_or(false)
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

/// Cursor for oldest-first recent-candidate scans used by fast promotion.
/// The composite key is deterministic because `(account_id, nonce)` is unique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentCandidateCursor {
    pub last_status_timestamp: DateTime<Utc>,
    pub last_account_id: String,
    pub last_nonce: u64,
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
    pub retained: u64,
    pub discarded: u64,
}

/// Point-in-time saturation snapshot of a backend's connection pool.
/// Returned by [`StorageBackend::pool_status`] for backends that pool
/// connections (Postgres/deadpool); the metrics refresher publishes it
/// as gauges so operators can see pool exhaustion building up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolStatus {
    /// Maximum number of connections the pool may hold.
    pub max_connections: u64,
    /// Connections currently managed by the pool (in use + idle).
    pub connections: u64,
    /// Idle connections ready to be acquired.
    pub available: u64,
    /// Tasks currently waiting to acquire a connection.
    pub pending_acquires: u64,
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

/// Identity of the canonicalization lease a worker holds while writing.
/// Fenced backends (Postgres) validate this against the `worker_leases`
/// row at the protected write boundary. A write that already validated its
/// lease may finish during a leadership transfer; account locks and conditional
/// mutations keep overlapping work safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseFence {
    pub lease_name: String,
    pub holder_id: String,
    pub fence_token: i64,
}

/// Outcome of a fenced canonicalization write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalWrite {
    /// The write committed.
    Applied,
    /// The caller's lease is no longer current; nothing was written.
    StaleLease,
    /// The target delta is no longer in candidate status (another owner
    /// already promoted or discarded it); nothing was written.
    NotCandidate,
}

/// Outcome of recording a client abandon request on a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbandonIntent {
    /// The request timestamp was recorded; the delta remains a candidate.
    Recorded,
    /// The candidate already carries an abandon request; nothing written.
    AlreadyRequested { requested_at: String },
    /// The delta is absent or no longer a candidate; nothing written.
    NotCandidate,
}

/// Outcome of a fenced candidate promotion. Promotion overwrites the
/// account state, so it carries one gate the other canonicalization
/// writes do not: the stored state must still sit at the candidate's
/// base commitment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromoteWrite {
    /// The promotion committed.
    Applied,
    /// The caller's lease is no longer current; nothing was written.
    StaleLease,
    /// The target delta is no longer in candidate status (another owner
    /// already promoted or discarded it); nothing was written.
    NotCandidate,
    /// The stored state no longer sits at the candidate's base
    /// commitment — a concurrent writer advanced or reconfigured the
    /// account after this pass read it; nothing was written. The
    /// candidate survives for the next pass, whose fresh verification
    /// against the moved base takes the divergence path if the delta
    /// is truly unsatisfiable.
    StaleBase,
}

/// Outcome of a candidate submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateSubmission {
    /// The candidate row and the pending-candidate flag committed.
    Submitted,
    /// The account already has a pending candidate, or a delta already
    /// occupies this nonce; nothing was written. The service layer maps
    /// this to the same conflict rejection as its pre-commit gate — the
    /// in-transaction recheck is what makes that gate race-proof.
    Conflict,
    /// The account state advanced after the service-layer validation but
    /// before the candidate transaction acquired the account lock.
    CommitmentMismatch { expected: String },
}

/// The exact lifecycle row a promotion is allowed to flip. The gate
/// must match the row the pass verified — not the union of promotable
/// kinds — because a client submission can supersede a retained or
/// client-abandoned row at its nonce mid-pass: a union gate would let
/// the promotion stamp the client's brand-new candidate canonical with
/// the old delta's verified commitment. An exact gate makes the
/// superseded promotion a no-op (`NotCandidate`) instead. Candidate
/// rows cannot be superseded (only the lease-holding worker deletes
/// them), so the candidate arm is as safe as it always was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotableKind {
    Candidate,
    Retained,
    ClientAbandoned,
}

impl PromotableKind {
    /// The promotable kind of the row a verification read, derived from
    /// its pre-promotion status.
    pub fn of(status: &DeltaStatus) -> Self {
        if status.is_retained() {
            PromotableKind::Retained
        } else if status.is_client_abandoned() {
            PromotableKind::ClientAbandoned
        } else {
            PromotableKind::Candidate
        }
    }

    /// Whether a stored status still is the row this promotion targets.
    pub fn matches(&self, status: &DeltaStatus) -> bool {
        match self {
            PromotableKind::Candidate => status.is_candidate(),
            PromotableKind::Retained => status.is_retained(),
            PromotableKind::ClientAbandoned => status.is_client_abandoned(),
        }
    }
}

/// Everything a candidate promotion commits as one unit: the advanced
/// account state, the delta flipped to canonical, the optional cosigner
/// auth sync, and the pending-candidate flag release. `now` stamps the
/// metadata `updated_at`; `fence` is `None` only for single-process
/// electors, whose leases have no shared-store row to validate — a
/// fencing backend refuses an unfenced promotion outright. `source` is
/// the lifecycle kind of the row the pass verified; the write only
/// flips a row still in that exact kind.
#[derive(Debug, Clone)]
pub struct CandidatePromotion {
    pub state: StateObject,
    pub delta: DeltaObject,
    pub new_auth: Option<crate::metadata::Auth>,
    pub now: String,
    pub fence: Option<LeaseFence>,
    pub source: PromotableKind,
}

/// Single-process fallback for [`StorageBackend::submit_candidate`]:
/// the delta write and the flag set are separate commits, and the
/// pending-candidate recheck stays at the service-layer gate — both
/// acceptable only where no concurrent task can observe the gap. Its
/// sole consumer is the cfg-gated mock backend — the filesystem backend
/// overrides the method with a locked sequence (issue #345 exposed
/// worker/API task interleavings) — hence the dead-code allowance for
/// non-test builds.
#[allow(dead_code)]
pub(crate) async fn submit_candidate_sequential(
    storage: &dyn StorageBackend,
    metadata: &dyn crate::metadata::MetadataStore,
    delta: &DeltaObject,
    now: &str,
) -> Result<CandidateSubmission, String> {
    // A retained row (issue #345) or a client-abandoned discard
    // (issue #319) is a best-effort recovery/history artifact, never
    // settled canonical history: a fresh candidate at its nonce
    // supersedes it — the client just re-supplied its intent for that
    // slot. Without the abandoned-row supersede, the very resubmission
    // the abandon endpoint exists to enable would be refused forever at
    // the nonce's unique constraint.
    if let Ok(existing) = storage.pull_delta(&delta.account_id, delta.nonce).await
        && (existing.status.is_retained() || existing.status.is_client_abandoned())
    {
        storage.delete_delta(&delta.account_id, delta.nonce).await?;
    }
    storage.submit_delta(delta).await?;
    metadata
        .set_has_pending_candidate(&delta.account_id, true, now)
        .await?;
    Ok(CandidateSubmission::Submitted)
}

/// Single-process fallback for [`StorageBackend::promote_candidate`]:
/// sequential writes with no fence, mirroring the pre-lease worker. The
/// base-commitment gate is a plain read-then-write, acceptable only
/// where no concurrent task can interleave. Its sole consumer is the
/// cfg-gated mock backend — the filesystem backend overrides the method
/// with a locked sequence (issue #345 exposed worker/API task
/// interleavings) — hence the dead-code allowance for non-test builds.
#[allow(dead_code)]
pub(crate) async fn promote_candidate_sequential(
    storage: &dyn StorageBackend,
    metadata: &dyn crate::metadata::MetadataStore,
    promotion: CandidatePromotion,
) -> Result<PromoteWrite, String> {
    // Source-kind gate: the row must still be the one the pass
    // verified. A concurrent submission superseding a retained or
    // client-abandoned row at this nonce makes the promotion a no-op.
    // A read error keeps the historical plain-write path (mocks
    // without a canned read).
    if let Ok(existing) = storage
        .pull_delta(&promotion.state.account_id, promotion.delta.nonce)
        .await
        && !promotion.source.matches(&existing.status)
    {
        return Ok(PromoteWrite::NotCandidate);
    }

    let current_state = storage.pull_state(&promotion.state.account_id).await?;
    if current_state.commitment != promotion.delta.prev_commitment {
        return Ok(PromoteWrite::StaleBase);
    }
    storage.submit_state(&promotion.state).await?;
    if let Some(new_auth) = promotion.new_auth {
        metadata
            .update_auth(&promotion.state.account_id, new_auth, &promotion.now)
            .await?;
    }
    storage.submit_delta(&promotion.delta).await?;
    metadata
        .clear_pending_candidate_if_none(&promotion.state.account_id, &promotion.now)
        .await?;
    Ok(PromoteWrite::Applied)
}

/// Single-process fallback for [`StorageBackend::discard_candidate`].
/// A flag-clear failure is tolerated (warn) to match the historical
/// discard path: the candidate row is already gone and the stuck flag
/// only costs an empty worker pass, never a wedge.
pub(crate) async fn discard_candidate_sequential(
    storage: &dyn StorageBackend,
    metadata: &dyn crate::metadata::MetadataStore,
    account_id: &str,
    nonce: u64,
    kind: DeltaStatusKind,
    now: &str,
) -> Result<CanonicalWrite, String> {
    // Guard the delete on the expected lifecycle kind so a stale discard
    // can never remove a row that was promoted meanwhile. A read error
    // keeps the historical plain-delete path (mocks without a canned
    // read; a missing row makes the delete itself the no-op/error).
    if let Ok(existing) = storage.pull_delta(account_id, nonce).await
        && DeltaStatusKind::of(&existing.status) != kind
    {
        return Ok(CanonicalWrite::NotCandidate);
    }
    storage.delete_delta(account_id, nonce).await?;
    if let Err(e) = metadata
        .clear_pending_candidate_if_none(account_id, now)
        .await
    {
        tracing::warn!(
            account_id = %account_id,
            error = %e,
            "Failed to clear has_pending_candidate flag after discard"
        );
    }
    Ok(CanonicalWrite::Applied)
}

/// Single-process fallback for [`StorageBackend::update_candidate_status`].
/// Best-effort merge of a concurrently recorded abandon request: the read
/// and the write are separate steps here, acceptable only where no
/// concurrent replica can observe the gap. Its sole consumer is the
/// cfg-gated mock backend — the filesystem backend overrides the method
/// with a locked read-modify-write — hence the dead-code allowance for
/// non-test builds.
#[allow(dead_code)]
pub(crate) async fn update_candidate_status_sequential(
    storage: &dyn StorageBackend,
    account_id: &str,
    nonce: u64,
    status: DeltaStatus,
) -> Result<CanonicalWrite, String> {
    let status = match storage.pull_delta(account_id, nonce).await {
        // A concurrently recorded abandon intent must not be wiped into
        // a retained status (no field to carry it): refuse the flip so
        // the next worker tick resolves the abandon instead.
        Ok(current) if status.is_retained() && current.status.abandon_requested_at().is_some() => {
            return Ok(CanonicalWrite::NotCandidate);
        }
        Ok(current) if current.status.is_candidate() => {
            status.with_abandon_request_preserved_from(current.status.abandon_requested_at())
        }
        Ok(_) => return Ok(CanonicalWrite::NotCandidate),
        // Mocks without a canned read keep the historical plain-write path.
        Err(_) => status,
    };
    storage
        .update_delta_status(account_id, nonce, status)
        .await?;
    Ok(CanonicalWrite::Applied)
}
pub(crate) mod encryption;
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

    /// Connection-pool saturation snapshot, for backends that pool
    /// connections. `None` (the default) for backends without one —
    /// the filesystem backend has no pool to report on.
    fn pool_status(&self) -> Option<PoolStatus> {
        None
    }

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
    /// Candidate deltas for one account, nonce-ascending. The default
    /// filters the full history in memory; backends with a typed status
    /// column override it so only candidate rows leave the store —
    /// candidates are a handful per account while the full history
    /// grows unboundedly.
    async fn pull_candidate_deltas(&self, account_id: &str) -> Result<Vec<DeltaObject>, String> {
        let mut deltas: Vec<DeltaObject> = self
            .pull_deltas_after(account_id, 0)
            .await?
            .into_iter()
            .filter(|delta| delta.status.is_candidate())
            .collect();
        deltas.sort_by_key(|delta| delta.nonce);
        Ok(deltas)
    }
    /// One oldest-first page of candidate deltas newer than `since` and after
    /// `cursor`. Backends apply both predicates before returning payloads.
    async fn pull_recent_candidate_deltas(
        &self,
        since: DateTime<Utc>,
        cursor: Option<&RecentCandidateCursor>,
        limit: u32,
    ) -> Result<Vec<DeltaObject>, String>;

    /// Recoverable deltas (issue #345) for one account, nonce-ascending
    /// — the background reconcile pass's read. Recoverable means every
    /// `retained` row, plus `discarded { client_abandoned }` rows whose
    /// status timestamp is at or after `abandoned_since`: the abandon
    /// quarantine (issue #319) cannot fully rule out a late-landing
    /// transaction, and one that lands after the abandon finalizes
    /// leaves the stored state behind the chain — the abandoned row
    /// holds everything needed to recover. The cutoff keeps the
    /// abandoned side bounded: old history rows are kept forever but
    /// must not be rescanned forever. The default filters the full
    /// history in memory; backends with a typed status column override
    /// it so only recoverable rows leave the store.
    async fn pull_recoverable_deltas(
        &self,
        account_id: &str,
        abandoned_since: DateTime<Utc>,
    ) -> Result<Vec<DeltaObject>, String> {
        let mut deltas: Vec<DeltaObject> = self
            .pull_deltas_after(account_id, 0)
            .await?
            .into_iter()
            .filter(|delta| is_recoverable(&delta.status, abandoned_since))
            .collect();
        deltas.sort_by_key(|delta| delta.nonce);
        Ok(deltas)
    }

    /// Accounts that currently hold at least one recoverable delta
    /// (issue #345): any `retained` row, or a client-abandoned discard
    /// at or after `abandoned_since`. These rows do not set the
    /// `has_pending_candidate` flag — by design, so they never re-lock
    /// the account — so the reconcile pass needs its own scan to find
    /// them.
    async fn list_accounts_with_recoverable_deltas(
        &self,
        abandoned_since: DateTime<Utc>,
    ) -> Result<Vec<String>, String>;

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
    async fn pull_all_delta_proposals(
        &self,
        account_id: &str,
    ) -> Result<Vec<ProposalRecord>, String>;
    async fn pull_pending_proposals(
        &self,
        account_id: &str,
    ) -> Result<Vec<ProposalRecord>, String> {
        let mut proposals = self.pull_all_delta_proposals(account_id).await?;
        proposals.retain(|record| record.proposal.status.is_pending());
        proposals.sort_by_key(|record| record.proposal.nonce);
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
    /// Atomically record a client's abandon request (issue #319) on the
    /// candidate at `nonce`: sets `abandon_requested_at` while preserving
    /// every worker-owned counter, only while the delta is still a
    /// candidate. Unfenced by design — the write is a non-destructive
    /// annotation; the lease-holding worker resolves the intent. An
    /// existing request timestamp is kept so retries cannot restart the
    /// abandon quarantine.
    async fn request_candidate_abandon(
        &self,
        account_id: &str,
        nonce: u64,
        now: &str,
    ) -> Result<AbandonIntent, String>;
    async fn update_delta_status(
        &self,
        account_id: &str,
        nonce: u64,
        status: DeltaStatus,
    ) -> Result<(), String>;

    // ----------------------------------------------------------------------
    // Canonicalization lifecycle writes. Deliberately required (no
    // defaults) on every implementation: a trait default silently
    // absorbing a dropped backend override would revert the Postgres
    // backend to unfenced, non-atomic writes. Postgres commits each of
    // these as one fenced transaction; single-process backends use the
    // `*_sequential` helpers above.
    // ----------------------------------------------------------------------

    /// Persist a new candidate delta and set the account's
    /// `has_pending_candidate` flag as one unit. Without atomicity a
    /// crash between the two writes leaves a candidate row the worker
    /// never selects (flag false) while new submissions stay rejected
    /// (row exists): a permanent wedge. Fencing backends additionally
    /// recheck under the account lock that no candidate exists and that
    /// the nonce is unoccupied, returning
    /// [`CandidateSubmission::Conflict`] instead of overwriting — two
    /// racing submissions that both passed the service-layer gate must
    /// not both commit, and a delayed submission must never overwrite
    /// canonical history at its nonce.
    async fn submit_candidate(
        &self,
        metadata: &dyn crate::metadata::MetadataStore,
        delta: &DeltaObject,
        now: &str,
    ) -> Result<CandidateSubmission, String>;

    /// Promote a verified candidate: advance the account state, sync
    /// cosigner auth when supplied, flip the delta to canonical, and
    /// release the pending-candidate flag — all or nothing, gated on
    /// the promotion's lease fence, on the delta still being a
    /// candidate, and on the stored state still sitting at the
    /// candidate's base commitment.
    async fn promote_candidate(
        &self,
        metadata: &dyn crate::metadata::MetadataStore,
        promotion: CandidatePromotion,
    ) -> Result<PromoteWrite, String>;

    /// Discard an unsatisfiable delta: delete the row only if it is
    /// still in the expected lifecycle `kind` (a row another owner
    /// already promoted to canonical must survive a stale discard) and
    /// release the pending-candidate flag, gated on the lease fence.
    /// `kind` is [`DeltaStatusKind::Candidate`] for the classic worker
    /// discard and [`DeltaStatusKind::Retained`] for the TTL expiry of
    /// a retained delta (issue #345).
    async fn discard_candidate(
        &self,
        metadata: &dyn crate::metadata::MetadataStore,
        account_id: &str,
        nonce: u64,
        kind: DeltaStatusKind,
        now: &str,
        fence: Option<&LeaseFence>,
    ) -> Result<CanonicalWrite, String>;

    /// Update a candidate's status (retry / divergence bookkeeping) only
    /// while it is still a candidate, gated on the lease fence — a stale
    /// worker's delayed update must never demote a canonical delta back
    /// to candidate.
    async fn update_candidate_status(
        &self,
        account_id: &str,
        nonce: u64,
        status: DeltaStatus,
        fence: Option<&LeaseFence>,
    ) -> Result<CanonicalWrite, String>;

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

    /// Per-account canonical delta history paginated newest-first
    /// by `nonce DESC` (issue #413). Client-facing counterpart of
    /// [`Self::list_account_deltas_paged`] restricted to `canonical`
    /// rows — the confirmed history a wallet renders after recovery.
    async fn list_canonical_deltas_paged(
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
