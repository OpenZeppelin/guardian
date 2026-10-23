//! Guardian execution of threshold-met proposals, as the base client reports it.

pub use guardian_shared::execution::{
    ExecutionFailureCode, ExecutionState, ExpirationBound, ForeignAccountUnavailableReason,
    RequestInvalidReason,
};

use crate::error::{ClientError, ClientResult};
use crate::proto;

/// Why an execution failed: a stable, typed cause and a user-safe message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionFailure {
    pub code: ExecutionFailureCode,
    pub message: String,
}

/// One execution of a proposal, identified by `(account_id, proposal_id)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalExecution {
    pub account_id: String,
    pub proposal_id: String,
    pub state: ExecutionState,
    /// Present exactly when `state` is `failed`.
    pub error: Option<ExecutionFailure>,
    /// The delta nonce the execution committed to, once it crossed the no-retry boundary.
    pub delta_nonce: Option<u64>,
    /// Whether this request started the execution, rather than finding it already running.
    pub newly_accepted: bool,
    /// Whether the proposal is still stored. A fact, not retry advice.
    pub proposal_exists: bool,
    /// Stored signatures the server ignored as invalid, duplicate or not from a cosigner.
    pub ignored_signatures: u32,
    pub updated_at: String,
}

impl TryFrom<proto::ExecutionEnvelope> for ProposalExecution {
    type Error = ClientError;

    fn try_from(envelope: proto::ExecutionEnvelope) -> ClientResult<Self> {
        let state = ExecutionState::parse(&envelope.state).map_err(ClientError::InvalidResponse)?;
        let error = envelope
            .error
            .map(|error| -> ClientResult<ExecutionFailure> {
                let meta = error
                    .meta_json
                    .as_deref()
                    .map(serde_json::from_str::<serde_json::Value>)
                    .transpose()?;
                let code = ExecutionFailureCode::from_parts(&error.code, meta.as_ref())
                    .map_err(ClientError::InvalidResponse)?;
                Ok(ExecutionFailure {
                    code,
                    message: error.message,
                })
            })
            .transpose()?;
        if (state == ExecutionState::Failed) != error.is_some() {
            return Err(ClientError::InvalidResponse(format!(
                "an execution in state '{}' {} an error",
                state.as_str(),
                if error.is_some() { "carries" } else { "lacks" }
            )));
        }
        Ok(Self {
            account_id: envelope.account_id,
            proposal_id: envelope.proposal_id,
            state,
            error,
            delta_nonce: envelope.delta_nonce,
            newly_accepted: envelope.newly_accepted,
            proposal_exists: envelope.proposal_exists,
            ignored_signatures: envelope.ignored_signatures,
            updated_at: envelope.updated_at,
        })
    }
}

pub(crate) fn required(
    envelope: Option<proto::ExecutionEnvelope>,
) -> ClientResult<ProposalExecution> {
    envelope
        .ok_or_else(|| {
            ClientError::InvalidResponse("the response carries no execution".to_string())
        })?
        .try_into()
}
