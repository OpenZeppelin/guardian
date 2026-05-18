//! Structured-log `Auditor` implementation (FR-021).
//!
//! Selected when Guardian is built without the `postgres` feature, OR
//! invoked by `PostgresAuditor` as a fallback when a row INSERT fails
//! mid-request (FR-027). Emits one `tracing::warn!` line per event
//! under `target = "audit.admin_action"`; field names match the
//! `admin_actions` column names 1:1 so log consumers and SQL
//! consumers see the same shape.

use chrono::Utc;

use super::{AuditEvent, Auditor};

/// Audit writer that emits one structured log line per event under
/// `target = "audit.admin_action"`. Used as the primary writer in
/// non-Postgres deployments and as the fallback writer when a
/// Postgres INSERT fails.
#[derive(Debug, Default, Clone)]
pub struct LogAuditor;

impl LogAuditor {
    pub const TARGET: &'static str = "audit.admin_action";

    pub const fn new() -> Self {
        Self
    }
}

impl Auditor for LogAuditor {
    fn record(&self, event: AuditEvent) {
        // `occurred_at` is assigned server-side at write time. On the
        // log path that means here, not the DB. Field names mirror the
        // Postgres columns so log consumers and `psql` consumers see
        // the same shape.
        let payload = serde_json::to_string(&event.payload).unwrap_or_else(|_| "{}".to_string());
        tracing::warn!(
            target: "audit.admin_action",
            occurred_at = %Utc::now().to_rfc3339(),
            operator_identity = %event.operator_identity,
            action_kind = %event.action_kind,
            target_account_id = ?event.target_account_id,
            payload = %payload,
            outcome = %event.outcome.as_str(),
            error_code = ?event.error_code,
            client_ip = ?event.client_ip,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{AuditOutcome, kinds};
    use serde_json::json;
    use tracing::Level;
    use tracing_subscriber::fmt::MakeWriter;

    /// `MakeWriter` that appends every emitted byte to a shared
    /// `Vec<u8>`. Enough to assert the emitted line carries the
    /// expected fields without depending on a real subscriber.
    #[derive(Clone, Default)]
    struct CapturedWriter {
        buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl CapturedWriter {
        fn contents(&self) -> String {
            String::from_utf8(self.buf.lock().unwrap().clone()).unwrap()
        }
    }

    impl std::io::Write for CapturedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.buf.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for CapturedWriter {
        type Writer = CapturedWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn capture(events: impl FnOnce()) -> String {
        let writer = CapturedWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer.clone())
            .with_max_level(Level::TRACE)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, events);
        writer.contents()
    }

    #[test]
    fn emits_one_line_per_event_under_audit_target() {
        let captured = capture(|| {
            LogAuditor::new().record(AuditEvent {
                operator_identity: "0xabc".into(),
                action_kind: kinds::AUTH_DENIED,
                target_account_id: None,
                payload: json!({
                    "route_path": "/dashboard/_authz_probe",
                    "http_method": "POST",
                    "required_permissions": ["accounts:pause"],
                }),
                outcome: AuditOutcome::Denied,
                error_code: Some("GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION".into()),
                client_ip: Some("203.0.113.5".into()),
            });
        });

        assert!(
            captured.contains("audit.admin_action"),
            "expected audit.admin_action target in: {captured}"
        );
        assert!(
            captured.contains("operator_identity") && captured.contains("0xabc"),
            "expected operator_identity in: {captured}"
        );
        assert!(
            captured.contains("auth.denied"),
            "expected action_kind in: {captured}"
        );
        assert!(
            captured.contains("denied"),
            "expected outcome in: {captured}"
        );
        assert!(
            captured.contains("GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION"),
            "expected error_code in: {captured}"
        );
        assert!(
            captured.contains("client_ip") && captured.contains("203.0.113.5"),
            "expected client_ip in: {captured}"
        );
        // Should be exactly one event in the output.
        assert_eq!(
            captured.matches("audit.admin_action").count(),
            1,
            "expected one audit line, got: {captured}"
        );
    }

    #[test]
    fn success_event_omits_error_code() {
        let captured = capture(|| {
            LogAuditor::new().record(AuditEvent {
                operator_identity: "0xdef".into(),
                action_kind: kinds::PROBE_ACCESS,
                target_account_id: None,
                payload: json!({}),
                outcome: AuditOutcome::Success,
                error_code: None,
                client_ip: None,
            });
        });
        assert!(captured.contains("probe.access"));
        assert!(captured.contains("success"));
        // `None` formats as `None` via the `?` formatter; that's the
        // expected representation when no error code applies.
        assert!(
            captured.contains("error_code=None"),
            "expected None error_code in: {captured}",
        );
        // client_ip is optional and absent for synthetic callers.
        assert!(
            captured.contains("client_ip=None"),
            "expected None client_ip in: {captured}",
        );
    }

    #[test]
    fn outcome_strings_match_admin_actions_check_constraint() {
        // `admin_actions.outcome` has `CHECK (outcome IN ('success','denied'))`.
        assert_eq!(AuditOutcome::Success.as_str(), "success");
        assert_eq!(AuditOutcome::Denied.as_str(), "denied");
    }
}
