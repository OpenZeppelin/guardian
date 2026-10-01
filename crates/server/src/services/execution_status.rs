use serde::Serialize;

pub use guardian_shared::execution::ExecutionState;

use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::services::resolve_account;
use crate::state::AppState;
use crate::storage::{ExecutionFailure, ExecutionPhase, ExecutionRecord, ExecutionTerminal};

/// The reported state an execution record derives: pre-terminal states are never stored.
pub fn state_of(record: &ExecutionRecord) -> ExecutionState {
    match &record.outcome {
        Some(outcome) => match outcome.terminal {
            ExecutionTerminal::Committed => ExecutionState::Committed,
            ExecutionTerminal::Failed { .. } => ExecutionState::Failed,
        },
        None if record.boundary_crossed() => ExecutionState::Submitted,
        None => match record.reservation.phase {
            ExecutionPhase::Accepted
            | ExecutionPhase::Verified
            | ExecutionPhase::Acknowledged
            | ExecutionPhase::Executed => ExecutionState::Pending,
            ExecutionPhase::Proving | ExecutionPhase::Proved => ExecutionState::Proving,
            ExecutionPhase::SubmissionCommitted
            | ExecutionPhase::Sent
            | ExecutionPhase::Reconciling => ExecutionState::Submitted,
        },
    }
}

/// Why an execution failed, on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct ExecutionError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub meta: Option<serde_json::Value>,
}

impl From<&ExecutionFailure> for ExecutionError {
    fn from(failure: &ExecutionFailure) -> Self {
        Self {
            code: failure.code.as_str().to_string(),
            message: failure.message.clone(),
            meta: failure.code.meta(),
        }
    }
}

/// The execution envelope every execution operation returns.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct ExecutionEnvelope {
    pub account_id: String,
    pub proposal_id: String,
    pub state: ExecutionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ExecutionError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_nonce: Option<u64>,
    pub newly_accepted: bool,
    pub proposal_exists: bool,
    pub ignored_signatures: u32,
    pub updated_at: String,
}

impl ExecutionEnvelope {
    pub fn from_record(
        record: &ExecutionRecord,
        proposal_exists: bool,
        newly_accepted: bool,
    ) -> Self {
        let error = match record.outcome.as_ref().map(|outcome| &outcome.terminal) {
            Some(ExecutionTerminal::Failed { failure }) => Some(ExecutionError::from(failure)),
            Some(ExecutionTerminal::Committed) | None => None,
        };
        let updated_at = record
            .outcome
            .as_ref()
            .map(|outcome| outcome.resolved_at)
            .unwrap_or(record.reservation.updated_at);
        Self {
            account_id: record.reservation.account_id.clone(),
            proposal_id: record.reservation.proposal_id.clone(),
            state: state_of(record),
            error,
            delta_nonce: record
                .evidence
                .as_ref()
                .map(|evidence| evidence.candidate_nonce),
            newly_accepted,
            proposal_exists,
            ignored_signatures: record.reservation.ignored_signatures,
            updated_at: updated_at.to_rfc3339(),
        }
    }
}

pub(crate) async fn proposal_exists(
    state: &AppState,
    account_id: &str,
    proposal_id: &str,
) -> Result<bool> {
    match state
        .storage
        .pull_delta_proposal(account_id, proposal_id)
        .await
    {
        Ok(_) => Ok(true),
        Err(error) if crate::storage::is_storage_not_found(&error) => Ok(false),
        Err(error) => Err(GuardianError::StorageError(error)),
    }
}

/// The latest attempt of a proposal's execution.
pub async fn get_execution(
    state: &AppState,
    account_id: &str,
    proposal_id: &str,
    credentials: &Credentials,
) -> Result<ExecutionEnvelope> {
    resolve_account(state, account_id, credentials).await?;
    let exists = proposal_exists(state, account_id, proposal_id).await?;
    let record = state
        .storage
        .load_latest_execution(account_id, proposal_id)
        .await
        .map_err(GuardianError::StorageError)?;
    match record {
        Some(record) => Ok(ExecutionEnvelope::from_record(&record, exists, false)),
        None if exists => Err(GuardianError::ExecutionNotFound {
            account_id: account_id.to_string(),
            proposal_id: proposal_id.to_string(),
        }),
        None => Err(GuardianError::ProposalNotFound {
            account_id: account_id.to_string(),
            commitment: proposal_id.to_string(),
        }),
    }
}

/// The account's in-flight execution, if any. A terminal execution is not in flight.
pub async fn current_execution(
    state: &AppState,
    account_id: &str,
    credentials: &Credentials,
) -> Result<Option<ExecutionEnvelope>> {
    resolve_account(state, account_id, credentials).await?;
    let Some(record) = state
        .storage
        .load_active_execution(account_id)
        .await
        .map_err(GuardianError::StorageError)?
    else {
        return Ok(None);
    };
    let exists = proposal_exists(state, account_id, &record.reservation.proposal_id).await?;
    Ok(Some(ExecutionEnvelope::from_record(&record, exists, false)))
}
