//! The Guardian execution wire vocabulary shared by the server and every client: the reported
//! states and the stable failure codes with their structured `meta`.

use serde::{Deserialize, Serialize};

/// Stable codes of the refusals an execution request or a proposal creation can receive
/// synchronously, before any execution record exists.
pub mod refusal_codes {
    pub const PROVING_UNAVAILABLE: &str = "GUARDIAN_PROVING_UNAVAILABLE";
    pub const PROPOSAL_MISSING_TRANSACTION_REQUEST: &str =
        "GUARDIAN_PROPOSAL_MISSING_TRANSACTION_REQUEST";
    pub const PROPOSAL_NOT_READY: &str = "GUARDIAN_PROPOSAL_NOT_READY";
    pub const EXECUTION_CONFLICT: &str = "GUARDIAN_EXECUTION_CONFLICT";
    pub const EXECUTION_BUSY: &str = "GUARDIAN_EXECUTION_BUSY";
    pub const EXECUTION_NOT_FOUND: &str = "GUARDIAN_EXECUTION_NOT_FOUND";
    pub const PROPOSAL_REQUEST_TOO_LARGE: &str = "GUARDIAN_PROPOSAL_REQUEST_TOO_LARGE";
    pub const ACCOUNT_REQUEST_CAPACITY_EXCEEDED: &str =
        "GUARDIAN_ACCOUNT_REQUEST_CAPACITY_EXCEEDED";
}

/// The reported state of an execution: exactly five values, a state existing only where it
/// changes what the caller does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Pending,
    Proving,
    Submitted,
    Committed,
    Failed,
}

impl ExecutionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionState::Pending => "pending",
            ExecutionState::Proving => "proving",
            ExecutionState::Submitted => "submitted",
            ExecutionState::Committed => "committed",
            ExecutionState::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        Ok(match value {
            "pending" => ExecutionState::Pending,
            "proving" => ExecutionState::Proving,
            "submitted" => ExecutionState::Submitted,
            "committed" => ExecutionState::Committed,
            "failed" => ExecutionState::Failed,
            other => return Err(format!("unknown execution state '{other}'")),
        })
    }

    /// Whether the execution has finished, one way or the other.
    pub fn is_terminal(&self) -> bool {
        matches!(self, ExecutionState::Committed | ExecutionState::Failed)
    }
}

/// Why a proposal's stored request is structurally not Guardian-executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestInvalidReason {
    BoundBlockNotDeclared,
    AuthArgsMissing,
    ApprovalExpirationMissing,
    InputNotesNotPinned,
}

/// Which signed expiration bound the chain has reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpirationBound {
    Approval,
    Transaction,
}

/// Why a foreign account the transaction loads could not be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForeignAccountUnavailableReason {
    Private,
    Unavailable,
}

/// Stable cause of a failed execution, persisted with its structured meta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionFailureCode {
    BindingMismatch,
    StateMismatch,
    RequestCodec,
    ProtocolMismatch,
    RequestInvalid(RequestInvalidReason),
    ExpirationReached(ExpirationBound),
    ChainBehind,
    ChainInconsistent,
    NodeUnavailable,
    ForeignAccountUnavailable(ForeignAccountUnavailableReason),
    InsufficientFee,
    ProvingFailed,
    SealingFailed,
    ExpirationBeyondHorizon,
    AccountInadmissible,
    SubmissionRejected,
    CandidateDiscarded,
    Expired,
    LeaseExpired,
    Abandoned,
}

#[derive(Serialize, Deserialize)]
struct ReasonMeta<T> {
    reason: T,
}

#[derive(Serialize, Deserialize)]
struct BoundMeta {
    bound: ExpirationBound,
}

impl ExecutionFailureCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionFailureCode::BindingMismatch => "GUARDIAN_EXECUTION_BINDING_MISMATCH",
            ExecutionFailureCode::StateMismatch => "GUARDIAN_EXECUTION_STATE_MISMATCH",
            ExecutionFailureCode::RequestCodec => "GUARDIAN_EXECUTION_REQUEST_CODEC",
            ExecutionFailureCode::ProtocolMismatch => "GUARDIAN_EXECUTION_PROTOCOL_MISMATCH",
            ExecutionFailureCode::RequestInvalid(_) => "GUARDIAN_EXECUTION_REQUEST_INVALID",
            ExecutionFailureCode::ExpirationReached(_) => "GUARDIAN_EXECUTION_EXPIRATION_REACHED",
            ExecutionFailureCode::ChainBehind => "GUARDIAN_EXECUTION_CHAIN_BEHIND",
            ExecutionFailureCode::ChainInconsistent => "GUARDIAN_EXECUTION_CHAIN_INCONSISTENT",
            ExecutionFailureCode::NodeUnavailable => "GUARDIAN_EXECUTION_NODE_UNAVAILABLE",
            ExecutionFailureCode::ForeignAccountUnavailable(_) => {
                "GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE"
            }
            ExecutionFailureCode::InsufficientFee => "GUARDIAN_EXECUTION_INSUFFICIENT_FEE",
            ExecutionFailureCode::ProvingFailed => "GUARDIAN_EXECUTION_PROVING_FAILED",
            ExecutionFailureCode::SealingFailed => "GUARDIAN_EXECUTION_SEALING_FAILED",
            ExecutionFailureCode::ExpirationBeyondHorizon => {
                "GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON"
            }
            ExecutionFailureCode::AccountInadmissible => "GUARDIAN_EXECUTION_ACCOUNT_INADMISSIBLE",
            ExecutionFailureCode::SubmissionRejected => "GUARDIAN_EXECUTION_SUBMISSION_REJECTED",
            ExecutionFailureCode::CandidateDiscarded => "GUARDIAN_EXECUTION_CANDIDATE_DISCARDED",
            ExecutionFailureCode::Expired => "GUARDIAN_EXECUTION_EXPIRED",
            ExecutionFailureCode::LeaseExpired => "GUARDIAN_EXECUTION_LEASE_EXPIRED",
            ExecutionFailureCode::Abandoned => "GUARDIAN_EXECUTION_ABANDONED",
        }
    }

    /// The structured `meta` this code carries on the wire, if any.
    pub fn meta(&self) -> Option<serde_json::Value> {
        let value = match self {
            ExecutionFailureCode::RequestInvalid(reason) => {
                serde_json::to_value(ReasonMeta { reason: *reason })
            }
            ExecutionFailureCode::ForeignAccountUnavailable(reason) => {
                serde_json::to_value(ReasonMeta { reason: *reason })
            }
            ExecutionFailureCode::ExpirationReached(bound) => {
                serde_json::to_value(BoundMeta { bound: *bound })
            }
            ExecutionFailureCode::BindingMismatch
            | ExecutionFailureCode::StateMismatch
            | ExecutionFailureCode::RequestCodec
            | ExecutionFailureCode::ProtocolMismatch
            | ExecutionFailureCode::ChainBehind
            | ExecutionFailureCode::ChainInconsistent
            | ExecutionFailureCode::NodeUnavailable
            | ExecutionFailureCode::InsufficientFee
            | ExecutionFailureCode::ProvingFailed
            | ExecutionFailureCode::SealingFailed
            | ExecutionFailureCode::ExpirationBeyondHorizon
            | ExecutionFailureCode::AccountInadmissible
            | ExecutionFailureCode::SubmissionRejected
            | ExecutionFailureCode::CandidateDiscarded
            | ExecutionFailureCode::Expired
            | ExecutionFailureCode::LeaseExpired
            | ExecutionFailureCode::Abandoned => return None,
        };
        Some(value.expect("failure meta serializes"))
    }

    /// Rebuild a persisted code from its stored string and meta.
    pub fn from_parts(code: &str, meta: Option<&serde_json::Value>) -> Result<Self, String> {
        fn required<T: serde::de::DeserializeOwned>(
            code: &str,
            meta: Option<&serde_json::Value>,
        ) -> Result<T, String> {
            let meta = meta.ok_or_else(|| format!("{code} is stored without its meta"))?;
            serde_json::from_value(meta.clone())
                .map_err(|e| format!("{code} has malformed meta: {e}"))
        }
        Ok(match code {
            "GUARDIAN_EXECUTION_BINDING_MISMATCH" => ExecutionFailureCode::BindingMismatch,
            "GUARDIAN_EXECUTION_STATE_MISMATCH" => ExecutionFailureCode::StateMismatch,
            "GUARDIAN_EXECUTION_REQUEST_CODEC" => ExecutionFailureCode::RequestCodec,
            "GUARDIAN_EXECUTION_PROTOCOL_MISMATCH" => ExecutionFailureCode::ProtocolMismatch,
            "GUARDIAN_EXECUTION_REQUEST_INVALID" => ExecutionFailureCode::RequestInvalid(
                required::<ReasonMeta<RequestInvalidReason>>(code, meta)?.reason,
            ),
            "GUARDIAN_EXECUTION_EXPIRATION_REACHED" => {
                ExecutionFailureCode::ExpirationReached(required::<BoundMeta>(code, meta)?.bound)
            }
            "GUARDIAN_EXECUTION_CHAIN_BEHIND" => ExecutionFailureCode::ChainBehind,
            "GUARDIAN_EXECUTION_CHAIN_INCONSISTENT" => ExecutionFailureCode::ChainInconsistent,
            "GUARDIAN_EXECUTION_NODE_UNAVAILABLE" => ExecutionFailureCode::NodeUnavailable,
            "GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE" => {
                ExecutionFailureCode::ForeignAccountUnavailable(
                    required::<ReasonMeta<ForeignAccountUnavailableReason>>(code, meta)?.reason,
                )
            }
            "GUARDIAN_EXECUTION_INSUFFICIENT_FEE" => ExecutionFailureCode::InsufficientFee,
            "GUARDIAN_EXECUTION_PROVING_FAILED" => ExecutionFailureCode::ProvingFailed,
            "GUARDIAN_EXECUTION_SEALING_FAILED" => ExecutionFailureCode::SealingFailed,
            "GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON" => {
                ExecutionFailureCode::ExpirationBeyondHorizon
            }
            "GUARDIAN_EXECUTION_ACCOUNT_INADMISSIBLE" => ExecutionFailureCode::AccountInadmissible,
            "GUARDIAN_EXECUTION_SUBMISSION_REJECTED" => ExecutionFailureCode::SubmissionRejected,
            "GUARDIAN_EXECUTION_CANDIDATE_DISCARDED" => ExecutionFailureCode::CandidateDiscarded,
            "GUARDIAN_EXECUTION_EXPIRED" => ExecutionFailureCode::Expired,
            "GUARDIAN_EXECUTION_LEASE_EXPIRED" => ExecutionFailureCode::LeaseExpired,
            "GUARDIAN_EXECUTION_ABANDONED" => ExecutionFailureCode::Abandoned,
            other => return Err(format!("unknown execution failure code '{other}'")),
        })
    }
}
