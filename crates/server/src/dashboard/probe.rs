//! Cargo-feature-gated authorization-middleware probe endpoint.
//!
//! Feature 006-operator-authz §FR-027 / FR-028. Exists only to
//! exercise the authorization middleware end-to-end before
//! [#181](https://github.com/OpenZeppelin/guardian/issues/181) (Account
//! Pause) lands a real mutating consumer. The route is registered
//! exclusively under `#[cfg(feature = "authz-test-probe")]`; release
//! builds compile without it and return 404 for the path.
//!
//! The route declares required permission set `{accounts:pause}`. On
//! a successful call (i.e. the authz middleware allowed it) the
//! handler invokes the `Auditor` with `action_kind = probe.access`,
//! `outcome = success`, and returns `204 No Content` — no other side
//! effect.

use axum::Extension;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use serde_json::json;

use crate::audit::{AuditEvent, AuditOutcome, kinds};
use crate::dashboard::permissions::Permission;
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
///
/// The success-row payload mirrors the `auth.denied` payload shape
/// (route_path, http_method, required_permissions) so successful and
/// denied rows for the same route carry the same forensic context —
/// downstream consumers can correlate success/denial by route without
/// branching on `outcome`.
pub async fn handle(
    State(state): State<AppState>,
    Extension(operator): Extension<AuthenticatedOperator>,
    request: Request,
) -> Result<StatusCode> {
    let route_path = request.uri().path().to_owned();
    let http_method = request.method().as_str().to_owned();
    let client_ip = crate::middleware::client_ip::extract_client_ip(&request);
    state.auditor.record(AuditEvent {
        operator_identity: operator.operator_id.clone(),
        action_kind: kinds::PROBE_ACCESS,
        target_account_id: None,
        payload: json!({
            "route_path": route_path,
            "http_method": http_method,
            "required_permissions": [Permission::AccountsPause.as_str()],
        }),
        outcome: AuditOutcome::Success,
        error_code: None,
        client_ip,
    });
    Ok(StatusCode::NO_CONTENT)
}
