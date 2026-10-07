use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::delta_object::DeltaObject;
use crate::storage::LeaseFence;

pub use guardian_shared::execution::{
    ExecutionFailureCode, ExpirationBound, ForeignAccountUnavailableReason, RequestInvalidReason,
};

/// Lease name that fences one account's execution. Account-scoped so
/// executions for different accounts never contend on one lease row.
pub fn execution_lease_name(account_id: &str) -> String {
    format!("execution:{account_id}")
}

/// Refuses an execution write fenced by any lease other than the account's execution lease.
pub(crate) fn ensure_execution_lease(
    account_id: &str,
    fence: &super::LeaseFence,
) -> Result<(), String> {
    let expected = execution_lease_name(account_id);
    if fence.lease_name == expected {
        Ok(())
    } else {
        Err(format!(
            "execution writes must be fenced by lease '{expected}', got '{}'",
            fence.lease_name
        ))
    }
}

/// Internal progress of one execution attempt. Never on the wire; each phase
/// maps onto exactly one reported state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPhase {
    Accepted,
    Verified,
    Acknowledged,
    Executed,
    Proving,
    Proved,
    SubmissionCommitted,
    Sent,
    Reconciling,
}

impl ExecutionPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionPhase::Accepted => "accepted",
            ExecutionPhase::Verified => "verified",
            ExecutionPhase::Acknowledged => "acknowledged",
            ExecutionPhase::Executed => "executed",
            ExecutionPhase::Proving => "proving",
            ExecutionPhase::Proved => "proved",
            ExecutionPhase::SubmissionCommitted => "submission_committed",
            ExecutionPhase::Sent => "sent",
            ExecutionPhase::Reconciling => "reconciling",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "accepted" => Ok(ExecutionPhase::Accepted),
            "verified" => Ok(ExecutionPhase::Verified),
            "acknowledged" => Ok(ExecutionPhase::Acknowledged),
            "executed" => Ok(ExecutionPhase::Executed),
            "proving" => Ok(ExecutionPhase::Proving),
            "proved" => Ok(ExecutionPhase::Proved),
            "submission_committed" => Ok(ExecutionPhase::SubmissionCommitted),
            "sent" => Ok(ExecutionPhase::Sent),
            "reconciling" => Ok(ExecutionPhase::Reconciling),
            other => Err(format!("unknown execution phase '{other}'")),
        }
    }
}

/// The durable per-account claim that one execution attempt is in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReservation {
    pub account_id: String,
    pub proposal_id: String,
    pub attempt: u32,
    pub fence: LeaseFence,
    pub lease_expires_at: DateTime<Utc>,
    pub phase: ExecutionPhase,
    pub candidate_nonce: Option<u64>,
    pub ignored_signatures: u32,
    pub released_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ExecutionReservation {
    pub fn is_active(&self) -> bool {
        self.released_at.is_none()
    }

    pub fn is_owned_by(&self, fence: &LeaseFence) -> bool {
        self.fence == *fence
    }
}

/// Request to open a reservation. The attempt number is allocated by the
/// backend under the account lock, never by the caller.
#[derive(Debug, Clone)]
pub struct NewExecutionReservation {
    pub account_id: String,
    pub proposal_id: String,
    pub fence: LeaseFence,
    pub lease_expires_at: DateTime<Utc>,
    pub ignored_signatures: u32,
    pub now: DateTime<Utc>,
}

/// What the execution will reconcile against, recorded before the send. Its
/// existence is the no-retry boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionEvidence {
    pub account_id: String,
    pub proposal_id: String,
    pub attempt: u32,
    pub candidate_nonce: u64,
    pub transaction_id: String,
    pub expected_commitment: String,
    pub reference_block: u32,
    pub expiration_block: u32,
    pub base_commitment: String,
    pub committed_at: DateTime<Utc>,
}

/// A failed execution: its stable cause and a user-safe message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionFailure {
    pub code: ExecutionFailureCode,
    pub message: String,
}

impl ExecutionFailure {
    pub fn new(code: ExecutionFailureCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl Serialize for ExecutionFailure {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        StoredFailure {
            code: self.code.as_str().to_string(),
            message: self.message.clone(),
            meta: self.code.meta(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ExecutionFailure {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let stored = StoredFailure::deserialize(deserializer)?;
        let code = ExecutionFailureCode::from_parts(&stored.code, stored.meta.as_ref())
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            code,
            message: stored.message,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredFailure {
    code: String,
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<serde_json::Value>,
}

/// How an execution attempt ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ExecutionTerminal {
    Committed,
    Failed { failure: ExecutionFailure },
}

/// The persisted terminal outcome of one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionOutcome {
    pub account_id: String,
    pub proposal_id: String,
    pub attempt: u32,
    pub terminal: ExecutionTerminal,
    pub resolved_at: DateTime<Utc>,
}

/// Everything stored for one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub reservation: ExecutionReservation,
    pub evidence: Option<SubmissionEvidence>,
    pub outcome: Option<ExecutionOutcome>,
}

impl ExecutionRecord {
    pub fn boundary_crossed(&self) -> bool {
        self.evidence.is_some()
    }
}

/// The boundary commit: Guardian's own candidate and the evidence it will
/// reconcile against, admitted together under the reservation that
/// authorized them.
#[derive(Debug, Clone)]
pub struct CandidateAdmission {
    pub fence: LeaseFence,
    pub delta: DeltaObject,
    pub evidence: SubmissionEvidence,
    pub now: DateTime<Utc>,
}

/// A terminal failure written by the execution's current owner.
#[derive(Debug, Clone)]
pub struct ExecutionResolution {
    pub account_id: String,
    pub proposal_id: String,
    pub attempt: u32,
    pub fence: LeaseFence,
    pub failure: ExecutionFailure,
    pub now: DateTime<Utc>,
}

/// Outcome of opening a reservation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReservationWrite {
    Created {
        attempt: u32,
    },
    AlreadyReserved {
        holder_id: String,
        proposal_id: String,
    },
    CandidateExists,
    StaleLease,
}

/// Outcome of renewing an active reservation or advancing its phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationUpdate {
    Applied,
    StaleLease,
    NotActive,
}

/// Outcome of transferring a live reservation to a new owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimWrite {
    Claimed,
    /// The caller's expected holder or fence no longer matches the
    /// reservation, so ownership was not transferred.
    ClaimSuperseded,
    /// The claimant's own lease is not current.
    StaleLease,
    NotActive,
}

/// Outcome of the boundary commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionWrite {
    Admitted,
    /// The caller is not the active reservation's owner for this attempt.
    NotAuthorized,
    CandidateExists,
    /// A settled delta already occupies the candidate's nonce.
    NonceOccupied,
    /// The stored state no longer sits at the candidate's base commitment.
    StaleBase,
    StaleLease,
    /// The account was paused or released, read under the same lock as the write.
    AccountInactive,
}

/// Outcome of a terminal execution write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveWrite {
    Resolved,
    NotAuthorized,
    AlreadyResolved,
    StaleLease,
    /// A pre-boundary failure was written after the boundary, or a
    /// post-boundary resolution before it.
    WrongSideOfBoundary,
}

/// Outcome of settling an execution whose candidate a promotion already made canonical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleWrite {
    Settled,
    /// The account is not at the expected state, or the candidate is not canonical yet.
    NotPromoted,
    StaleLease,
    NotActive,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_name_is_account_scoped_and_never_the_canonicalization_lease() {
        let name = execution_lease_name("0xabc");
        assert_eq!(name, "execution:0xabc");
        assert_ne!(name, crate::coordination::CANONICALIZATION_LEASE);
        assert_ne!(execution_lease_name("a"), execution_lease_name("b"));
    }

    #[test]
    fn every_phase_round_trips_through_its_stored_name() {
        for phase in [
            ExecutionPhase::Accepted,
            ExecutionPhase::Verified,
            ExecutionPhase::Acknowledged,
            ExecutionPhase::Executed,
            ExecutionPhase::Proving,
            ExecutionPhase::Proved,
            ExecutionPhase::SubmissionCommitted,
            ExecutionPhase::Sent,
            ExecutionPhase::Reconciling,
        ] {
            assert_eq!(ExecutionPhase::parse(phase.as_str()).unwrap(), phase);
        }
        assert!(ExecutionPhase::parse("released").is_err());
    }

    #[test]
    fn failure_codes_round_trip_with_their_meta() {
        let codes = [
            ExecutionFailureCode::BindingMismatch,
            ExecutionFailureCode::RequestInvalid(RequestInvalidReason::BoundBlockNotDeclared),
            ExecutionFailureCode::RequestInvalid(RequestInvalidReason::InputNotesNotPinned),
            ExecutionFailureCode::ExpirationReached(ExpirationBound::Approval),
            ExecutionFailureCode::ExpirationReached(ExpirationBound::Transaction),
            ExecutionFailureCode::ForeignAccountUnavailable(
                ForeignAccountUnavailableReason::Private,
            ),
            ExecutionFailureCode::InsufficientSignatures,
            ExecutionFailureCode::SealingFailed,
            ExecutionFailureCode::AcknowledgementFailed,
            ExecutionFailureCode::Abandoned,
        ];
        for code in codes {
            let meta = code.meta();
            assert_eq!(
                ExecutionFailureCode::from_parts(code.as_str(), meta.as_ref()).unwrap(),
                code
            );
        }
    }

    #[test]
    fn failure_meta_uses_the_contract_keys_and_values() {
        assert_eq!(
            ExecutionFailureCode::RequestInvalid(RequestInvalidReason::AuthArgsMissing).meta(),
            Some(serde_json::json!({ "reason": "auth_args_missing" }))
        );
        assert_eq!(
            ExecutionFailureCode::ExpirationReached(ExpirationBound::Approval).meta(),
            Some(serde_json::json!({ "bound": "approval" }))
        );
        assert_eq!(
            ExecutionFailureCode::ForeignAccountUnavailable(
                ForeignAccountUnavailableReason::Unavailable
            )
            .meta(),
            Some(serde_json::json!({ "reason": "unavailable" }))
        );
        assert_eq!(ExecutionFailureCode::ChainBehind.meta(), None);
    }

    #[test]
    fn a_meta_bearing_code_without_its_meta_is_rejected() {
        assert!(
            ExecutionFailureCode::from_parts("GUARDIAN_EXECUTION_REQUEST_INVALID", None).is_err()
        );
        assert!(
            ExecutionFailureCode::from_parts("GUARDIAN_EXECUTION_ANCHOR_EXPIRED", None).is_err()
        );
    }

    #[test]
    fn stored_failures_serialize_as_code_message_and_meta() {
        let failure = ExecutionFailure {
            code: ExecutionFailureCode::ExpirationReached(ExpirationBound::Transaction),
            message: "expired".to_string(),
        };
        let json = serde_json::to_value(&failure).unwrap();
        assert_eq!(json["code"], "GUARDIAN_EXECUTION_EXPIRATION_REACHED");
        assert_eq!(json["meta"]["bound"], "transaction");
        let back: ExecutionFailure = serde_json::from_value(json).unwrap();
        assert_eq!(back, failure);
    }
}
