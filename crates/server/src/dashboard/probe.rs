//! Cargo-feature-gated authorization-middleware probe endpoint.
//!
//! Feature 006-operator-authz §FR-027 / FR-028. Exists only to
//! exercise the authorization middleware end-to-end before
//! [#181](https://github.com/OpenZeppelin/guardian/issues/181) (Account
//! Pause) lands a real mutating consumer. The route is registered
//! exclusively under `#[cfg(feature = "authz-probe")]`; release
//! builds compile without it and return 404 for the path.
//!
//! The route declares required permission set `{accounts:pause}`. On
//! a successful call (i.e. the authz middleware allowed it) the
//! handler invokes the `Auditor` with `action_kind = probe.access`,
//! `outcome = success`, and returns `204 No Content` — no other side
//! effect.

use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::json;

use crate::audit::{AuditEvent, AuditOutcome, kinds};
use crate::dashboard::types::AuthenticatedOperator;
use crate::error::Result;
use crate::state::AppState;

/// Stable URL path for the probe. Pinning the const here keeps the
/// production route registration and the test/smoke harness in sync.
pub const PROBE_PATH: &str = "/_authz_probe";

/// Handler for `POST /dashboard/_authz_probe`. Reached only when the
/// authorization middleware has already verified the caller holds
/// `{accounts:pause}`. Records one `admin_actions` event with
/// `action_kind = probe.access` then returns 204.
pub async fn handle(
    State(state): State<AppState>,
    Extension(operator): Extension<AuthenticatedOperator>,
) -> Result<StatusCode> {
    state.auditor.record(AuditEvent {
        operator_identity: operator.operator_id.clone(),
        action_kind: kinds::PROBE_ACCESS,
        target_account_id: None,
        payload: json!({}),
        outcome: AuditOutcome::Success,
        error_code: None,
    });
    Ok(StatusCode::NO_CONTENT)
}
