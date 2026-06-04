//! Unpause service. Mirror of [`crate::services::pause_account`].
//! Idempotent: a call against an already-active account is a no-op
//! at the persistence layer and still emits an `accounts.unpause`
//! audit row.

use serde::Serialize;
use serde_json::json;

use crate::audit::{AuditEvent, AuditOutcome, kinds};
use crate::dashboard::AuthenticatedOperator;
use crate::error::{GuardianError, Result};
use crate::services::account_status::{AccountStatus, PauseTransition};
use crate::services::pause_account::validate_reason;
use crate::state::AppState;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct UnpauseResponse {
    pub account_id: String,
    pub before_state: AccountStatus,
    pub after_state: AccountStatus,
    pub reason: Option<String>,
}

pub async fn unpause(
    state: &AppState,
    operator: &AuthenticatedOperator,
    account_id: &str,
    reason: Option<&str>,
    client_ip: Option<String>,
) -> Result<UnpauseResponse> {
    if let Some(r) = reason {
        validate_reason(r)?;
    }

    let transition: PauseTransition =
        state.metadata.clear_pause(account_id).await.map_err(|e| {
            if e.contains("Account not found") {
                GuardianError::AccountNotFound(account_id.to_string())
            } else {
                GuardianError::StorageError(format!("Failed to clear pause: {e}"))
            }
        })?;

    state.auditor.record(AuditEvent {
        operator_identity: operator.operator_id.clone(),
        action_kind: kinds::ACCOUNTS_UNPAUSE,
        target_account_id: Some(account_id.to_string()),
        payload: json!({
            "before_state": transition.before_state.as_str(),
            "after_state": transition.after_state.as_str(),
            "reason": reason,
        }),
        outcome: AuditOutcome::Success,
        error_code: None,
        client_ip,
    });

    Ok(UnpauseResponse {
        account_id: account_id.to_string(),
        before_state: transition.before_state,
        after_state: transition.after_state,
        reason: reason.map(|r| r.to_string()),
    })
}
