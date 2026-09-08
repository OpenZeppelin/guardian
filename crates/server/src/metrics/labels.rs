//! Centralized label *values*, as enums.
//!
//! [`super::names`] owns metric names and label keys; this module owns
//! the closed value sets those labels may carry. Call sites go through
//! these enums instead of string literals so a value can't drift from
//! the documented taxonomy (e.g. a help text advertising an event no
//! call site emits) and so the full set of values stays greppable in
//! one place.

/// Success/failure outcome shared by operation-style counters
/// (storage, operator auth, Miden RPC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Error,
}

impl Outcome {
    pub fn from_ok(ok: bool) -> Self {
        if ok { Self::Ok } else { Self::Error }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }
}

/// How one canonicalization pass ended
/// (`guardian_canonicalization_runs_total`). Per-account errors and
/// lease-loss cancellation do not fail the pass, so a plain ok/error
/// split would report degraded passes as healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// Every listed account was processed without error.
    Completed,
    /// The pass finished but at least one account failed.
    Partial,
    /// The pass stopped early because the lease was lost.
    Cancelled,
    /// The pass could not run at all (e.g. the account listing failed).
    Error,
}

impl RunOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
        }
    }
}

/// Multisig proposal lifecycle events (`guardian_proposals_total`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalEvent {
    /// A new proposal was stored (`push_delta_proposal`).
    Created,
    /// A cosigner signature was appended (`sign_delta_proposal`).
    Signed,
    /// The proposal's delta became canonical and the proposal left the
    /// queue. Emitted when finalization is detected, regardless of
    /// whether the cleanup delete succeeded.
    Finalized,
}

impl ProposalEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Signed => "signed",
            Self::Finalized => "finalized",
        }
    }
}

/// How a delta arrived (`guardian_deltas_submitted_total`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaKind {
    /// Pushed directly without a matching multisig proposal.
    Direct,
    /// Commit of a previously coordinated proposal.
    ProposalCommit,
}

impl DeltaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::ProposalCommit => "proposal_commit",
        }
    }
}

/// Per-candidate outcomes of the canonicalization worker
/// (`guardian_canonicalization_candidates_total`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateOutcome {
    Canonicalized,
    Retried,
    Discarded,
    GraceDeferred,
    /// The on-chain commitment matched neither the candidate's previous
    /// nor its expected new commitment, but not yet on enough consecutive
    /// ticks to discard; deferred for another confirmation.
    DivergenceDeferred,
    /// Discarded because the account advanced past the candidate's base
    /// state on-chain, making verification permanently unsatisfiable.
    Diverged,
    /// Discarded because the client abandoned it via the abandon-candidate
    /// endpoint (issue #319): the client knows its transaction will never
    /// land and releases the account instead of waiting out grace+retries.
    Abandoned,
    /// Promotion rolled back because the stored state moved off the
    /// candidate's base commitment during the pass; the candidate is
    /// re-verified against the new base next tick.
    StaleBase,
    /// Retry budget exhausted but the candidate was kept as `retained`
    /// (issue #345) for background reconciliation instead of deleted.
    Retained,
    /// A retained delta verified against the on-chain commitment on a
    /// later pass and was promoted to canonical — the stuck base
    /// auto-recovered.
    Reconciled,
    /// A retained delta's expected commitment was still not observed
    /// on-chain; kept for the next reconcile pass (until its TTL).
    ReconcileDeferred,
    /// A retained delta outlived its TTL without ever verifying and was
    /// dropped for good.
    ReconcileExpired,
}

impl CandidateOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Canonicalized => "canonicalized",
            Self::Retried => "retried",
            Self::Discarded => "discarded",
            Self::GraceDeferred => "grace_deferred",
            Self::DivergenceDeferred => "divergence_deferred",
            Self::Diverged => "diverged",
            Self::Abandoned => "abandoned",
            Self::StaleBase => "stale_base",
            Self::Retained => "retained",
            Self::Reconciled => "reconciled",
            Self::ReconcileDeferred => "reconcile_deferred",
            Self::ReconcileExpired => "reconcile_expired",
        }
    }
}

/// Which connection pool a `guardian_db_pool_*` gauge describes. The
/// server runs two independent Postgres pools with separately-tunable
/// sizes: `storage` (delta/state, canonicalization) and `metadata`
/// (account metadata, dashboard listings, operator auth, audit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolKind {
    Storage,
    Metadata,
}

impl PoolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Metadata => "metadata",
        }
    }
}

/// Account network kind (`guardian_accounts_created_total`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountKind {
    Miden,
    #[cfg(feature = "evm")]
    Evm,
}

impl AccountKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Miden => "miden",
            #[cfg(feature = "evm")]
            Self::Evm => "evm",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_lower_snake_case() {
        let all = [
            Outcome::Ok.as_str(),
            Outcome::Error.as_str(),
            RunOutcome::Completed.as_str(),
            RunOutcome::Partial.as_str(),
            RunOutcome::Cancelled.as_str(),
            RunOutcome::Error.as_str(),
            ProposalEvent::Created.as_str(),
            ProposalEvent::Signed.as_str(),
            ProposalEvent::Finalized.as_str(),
            DeltaKind::Direct.as_str(),
            DeltaKind::ProposalCommit.as_str(),
            CandidateOutcome::Canonicalized.as_str(),
            CandidateOutcome::Retried.as_str(),
            CandidateOutcome::Discarded.as_str(),
            CandidateOutcome::GraceDeferred.as_str(),
            CandidateOutcome::DivergenceDeferred.as_str(),
            CandidateOutcome::Diverged.as_str(),
            CandidateOutcome::Abandoned.as_str(),
            CandidateOutcome::StaleBase.as_str(),
            CandidateOutcome::Retained.as_str(),
            CandidateOutcome::Reconciled.as_str(),
            CandidateOutcome::ReconcileDeferred.as_str(),
            CandidateOutcome::ReconcileExpired.as_str(),
            AccountKind::Miden.as_str(),
            PoolKind::Storage.as_str(),
            PoolKind::Metadata.as_str(),
        ];
        for value in all {
            assert!(
                value.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "label value `{value}` must be lower snake_case"
            );
        }
    }

    #[test]
    fn outcome_from_ok_maps_correctly() {
        assert_eq!(Outcome::from_ok(true), Outcome::Ok);
        assert_eq!(Outcome::from_ok(false), Outcome::Error);
    }
}
