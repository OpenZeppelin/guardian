//! Authorization middleware for operator-permission gating.
//!
//! Feature 006-operator-authz §FR-008..FR-014. Runs **after**
//! [`crate::dashboard::middleware::require_dashboard_session`] (FR-012)
//! and denies requests whose authenticated operator does not hold the
//! full required permission set declared by the route (conjunction —
//! FR-011). Denials produce
//! [`crate::error::GuardianError::InsufficientOperatorPermission`]
//! (HTTP 403) and one audit row with `action_kind = auth.denied`.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use serde_json::json;

use crate::audit::{AuditEvent, AuditOutcome, kinds};
use crate::error::{GuardianError, Result};
use crate::state::AppState;

use super::permissions::Permission;
use super::types::AuthenticatedOperator;

/// Required-permission set declared by a single route. Compile-time
/// `&'static [Permission]` is sufficient for v1 because every route
/// declares its permissions at registration time, not dynamically.
pub type RequiredPermissions = &'static [Permission];

/// State bundle for the authorization middleware. Combines the
/// `AppState` (for the `Auditor` reference) with the route's required
/// permission set. Constructed at route-registration time and threaded
/// to the middleware via `axum::middleware::from_fn_with_state`.
#[derive(Clone)]
pub struct AuthzState {
    pub app_state: AppState,
    pub required: RequiredPermissions,
}

impl AuthzState {
    pub fn new(app_state: AppState, required: RequiredPermissions) -> Self {
        Self {
            app_state,
            required,
        }
    }
}

/// Authorization middleware function. Wire via:
/// ```ignore
/// .route_layer(axum::middleware::from_fn_with_state(
///     AuthzState::new(app_state.clone(), &[Permission::DashboardRead]),
///     crate::dashboard::authz::enforce,
/// ))
/// ```
/// Must run **after** [`require_dashboard_session`](crate::dashboard::middleware::require_dashboard_session)
/// (FR-012) so the `AuthenticatedOperator` is already in extensions.
pub async fn enforce(
    State(state): State<AuthzState>,
    request: Request,
    next: Next,
) -> Result<Response> {
    let AuthzState {
        app_state: state,
        required,
    } = state;
    // The session middleware (`require_dashboard_session`) runs first,
    // so `AuthenticatedOperator` MUST be present. If it's missing we
    // surface an internal error rather than 401 — that would indicate
    // a route was wired with authz but without session auth, which is
    // a programming bug, not a user error.
    let operator = request
        .extensions()
        .get::<AuthenticatedOperator>()
        .cloned()
        .ok_or_else(|| {
            GuardianError::ConfigurationError(
                "authorization middleware ran before session authentication".to_string(),
            )
        })?;

    let missing = missing_permissions(&operator.effective_permissions, required);
    if missing.is_empty() {
        return Ok(next.run(request).await);
    }

    // Audit MUST precede the response (FR-013); payload carries
    // route + method per FR-025. Pull `OriginalUri` so the path
    // reflects the full pre-nest form (`/dashboard/_authz_probe`)
    // an incident responder would actually curl.
    let route_path = request
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map(|uri| uri.0.path().to_owned())
        .unwrap_or_else(|| request.uri().path().to_owned());
    let http_method = request.method().as_str().to_owned();
    let required_strings: Vec<String> = required.iter().map(|p| p.as_str().to_owned()).collect();
    let client_ip = crate::middleware::client_ip::extract_client_ip(&request);
    state.auditor.record(AuditEvent {
        operator_identity: operator.operator_id.clone(),
        action_kind: kinds::AUTH_DENIED,
        target_account_id: None,
        payload: json!({
            "route_path": route_path,
            "http_method": http_method,
            "required_permissions": required_strings,
        }),
        outcome: AuditOutcome::Denied,
        error_code: Some("GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION".to_string()),
        client_ip,
    });

    Err(GuardianError::InsufficientOperatorPermission {
        missing_permissions: missing,
    })
}

/// Compute the set difference `required \ held`, returning a sorted
/// `Vec<String>` of the wire strings the operator lacks (FR-017
/// lexicographic ordering). Returns an empty vec when the operator
/// holds every required permission.
fn missing_permissions(
    held: &Arc<BTreeSet<Permission>>,
    required: RequiredPermissions,
) -> Vec<String> {
    let mut missing: BTreeSet<&'static str> = BTreeSet::new();
    for permission in required {
        if !held.contains(permission) {
            missing.insert(permission.as_str());
        }
    }
    missing.into_iter().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{AuditEvent, AuditOutcome};
    use crate::testing::helpers::CapturingAuditor;

    fn held(perms: &[Permission]) -> Arc<BTreeSet<Permission>> {
        Arc::new(perms.iter().copied().collect())
    }

    #[test]
    fn missing_permissions_returns_empty_when_all_held() {
        let result = missing_permissions(
            &held(&[Permission::DashboardRead, Permission::AccountsPause]),
            &[Permission::DashboardRead],
        );
        assert!(result.is_empty());
    }

    #[test]
    fn missing_permissions_returns_lexicographic_sorted_diff() {
        let result = missing_permissions(
            &held(&[Permission::DashboardRead]),
            &[
                Permission::PoliciesWrite,
                Permission::AccountsPause,
                Permission::DashboardRead,
            ],
        );
        assert_eq!(result, vec!["accounts:pause", "policies:write"]);
    }

    #[test]
    fn missing_permissions_dedupes_repeated_requirements() {
        let result = missing_permissions(
            &held(&[]),
            &[Permission::AccountsPause, Permission::AccountsPause],
        );
        assert_eq!(result, vec!["accounts:pause"]);
    }

    #[test]
    fn empty_required_set_always_passes() {
        // Defensive: a route declaring `&[]` should never deny, since
        // the operator is already authenticated. We don't expect this
        // pattern in production code but it's the natural identity.
        let result = missing_permissions(&held(&[]), &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn capturing_auditor_records_events_for_unit_tests() {
        use crate::audit::Auditor;
        let auditor = CapturingAuditor::default();
        auditor.record(AuditEvent {
            operator_identity: "0xabc".into(),
            action_kind: kinds::AUTH_DENIED,
            target_account_id: None,
            payload: json!({}),
            outcome: AuditOutcome::Denied,
            error_code: Some("GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION".into()),
            client_ip: None,
        });
        let snap = auditor.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].action_kind, kinds::AUTH_DENIED);
    }
}
