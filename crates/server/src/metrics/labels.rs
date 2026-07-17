//! Centralized label *values*, as enums.
//!
//! [`super::names`] owns metric names and label keys; this module owns
//! the closed value sets those labels may carry. Call sites go through
//! these enums instead of string literals so a value can't drift from
//! the documented taxonomy (e.g. a help text advertising an event no
//! call site emits) and so the full set of values stays greppable in
//! one place.

/// Success/failure outcome shared by operation-style counters
/// (storage, canonicalization runs, operator auth, Miden RPC).
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
