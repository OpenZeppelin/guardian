//! Pause service.
//!
//! After persistence succeeds, an audit event is dispatched to the
//! [`Auditor`](crate::audit). For the Postgres auditor this is a
//! fire-and-forget background insert (FR-027) that falls back to a
//! structured log on failure, so the row is not guaranteed to be
//! visible — or persisted — by the time the 200 response returns.
//! Re-pause is first-writer-wins: the persisted `paused_at` /
//! `paused_reason` are preserved and re-emitted on the response.

use serde::Serialize;
use serde_json::json;

use crate::audit::{AuditEvent, AuditOutcome, kinds};
use crate::dashboard::AuthenticatedOperator;
use crate::error::{GuardianError, Result};
use crate::services::account_status::{AccountStatus, PauseTransition};
use crate::state::AppState;

pub const MAX_REASON_LEN: usize = 512;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct PauseResponse {
    pub account_id: String,
    pub before_state: AccountStatus,
    pub after_state: AccountStatus,
    pub paused_at: String,
    pub paused_reason: String,
}

pub fn validate_reason(reason: &str) -> Result<()> {
    if reason.is_empty() {
        return Err(GuardianError::InvalidInput(
            "reason must be non-empty".to_string(),
        ));
    }
    if reason.chars().count() > MAX_REASON_LEN {
        return Err(GuardianError::InvalidInput(format!(
            "reason exceeds {MAX_REASON_LEN} character limit"
        )));
    }
    Ok(())
}

pub async fn pause(
    state: &AppState,
    operator: &AuthenticatedOperator,
    account_id: &str,
    reason: &str,
    client_ip: Option<String>,
) -> Result<PauseResponse> {
    validate_reason(reason)?;

    let transition: PauseTransition = state
        .metadata
        .set_pause(account_id, state.clock.now(), reason)
        .await
        .map_err(|e| {
            if e.contains("Account not found") {
                GuardianError::AccountNotFound(account_id.to_string())
            } else {
                GuardianError::StorageError(format!("Failed to set pause: {e}"))
            }
        })?;

    let paused_at = transition
        .paused_at
        .expect("set_pause must return paused_at on the paused branch");
    let paused_reason = transition
        .paused_reason
        .unwrap_or_else(|| reason.to_string());

    state.auditor.record(AuditEvent {
        operator_identity: operator.operator_id.clone(),
        action_kind: kinds::ACCOUNTS_PAUSE,
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

    Ok(PauseResponse {
        account_id: account_id.to_string(),
        before_state: transition.before_state,
        after_state: transition.after_state,
        paused_at: paused_at.to_rfc3339(),
        paused_reason,
    })
}
