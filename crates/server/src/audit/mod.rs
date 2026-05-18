//! Always-on audit writer for mutating operator actions.
//!
//! Feature: 006-operator-authz. The `Auditor` trait is the single
//! surface every audit event flows through (FR-019); each event lands
//! as either a row in the `admin_actions` Postgres table or a
//! structured `tracing::warn!` line under `target =
//! "audit.admin_action"` on filesystem-only deployments (FR-021).
//!
//! The `Auditor` is always invoked (FR-020). There is no feature flag
//! that disables it. Backend selection happens at build time in
//! `crates/server/src/builder/storage.rs` to mirror the existing
//! `MetadataStore` compile-time alternative (`postgres` feature).

use std::sync::Arc;

pub mod kinds;
mod log;

pub use log::LogAuditor;

#[cfg(feature = "postgres")]
mod postgres;

#[cfg(feature = "postgres")]
pub use postgres::PostgresAuditor;

/// One mutating-action attempt as it crosses the audit boundary. The
/// shape mirrors the `admin_actions` column set 1:1 (data-model.md
/// §Audit event). Server-controlled fields (`id`, `occurred_at`) are
/// assigned at persistence time and are not carried here.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    /// Stable operator identity (today the commitment hex string from
    /// the existing `AuthenticatedOperator::operator_id` field).
    pub operator_identity: String,
    /// Stable `action_kind` from `crates/server/src/audit/kinds.rs`.
    pub action_kind: &'static str,
    /// Account this action targeted, when applicable. Null for
    /// `auth.denied` / `probe.access`.
    pub target_account_id: Option<String>,
    /// Action context per the per-kind payload schema in
    /// `data-model.md`. Must not carry note contents, signatures, or
    /// any per-account secret state (FR-025).
    pub payload: serde_json::Value,
    /// `success` or `denied`. Matches the `admin_actions.outcome`
    /// CHECK constraint.
    pub outcome: AuditOutcome,
    /// Stable Guardian error code string when `outcome` is `denied`.
    /// `None` on success.
    pub error_code: Option<String>,
    /// Originating client IP. `None` when no request context is
    /// available (synthetic callers, fault-injection tests).
    pub client_ip: Option<String>,
}

/// The two outcome values pinned by the `admin_actions.outcome` CHECK
/// constraint. Kept as an enum so the writer side cannot accidentally
/// introduce a new value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    Success,
    Denied,
}

impl AuditOutcome {
    /// Stable wire string for the `admin_actions.outcome` column.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Denied => "denied",
        }
    }
}

/// Single audit-write surface. The middleware and consumer endpoints
/// call `record`; the rest of the codebase MUST NOT bypass this
/// trait. There is intentionally no method to update or delete an
/// event — append-only is enforced both at this surface (no method)
/// and at the Postgres trigger layer (data-model.md, research Decision 2).
pub trait Auditor: Send + Sync {
    fn record(&self, event: AuditEvent);
}

/// Convenience alias: middleware and request handlers carry the
/// Auditor as `Extension<SharedAuditor>` on the axum request.
pub type SharedAuditor = Arc<dyn Auditor>;
