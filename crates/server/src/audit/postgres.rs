//! Postgres-backed `Auditor` implementation (FR-021 — Postgres path).
//!
//! INSERTs one row into `admin_actions` per audit event. On transient
//! INSERT failure, falls through to the `LogAuditor` emission path so
//! the event is never invisible (FR-027). The fallback is intentional:
//! security-relevant events losing their row to a DB hiccup MUST still
//! surface to log scrapers.
//!
//! Available only with the `postgres` Cargo feature.

use std::sync::Arc;

use diesel::prelude::*;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use crate::schema::admin_actions;

use super::{AuditEvent, Auditor, LogAuditor};

/// Diesel insertable view of one `AuditEvent`. `id` and `occurred_at`
/// are DB-assigned; this struct does not carry them. The lifetime is
/// `'a` so the writer can borrow `event.action_kind` directly without
/// allocating.
#[derive(Insertable)]
#[diesel(table_name = admin_actions)]
struct NewAdminAction<'a> {
    operator_identity: &'a str,
    action_kind: &'a str,
    target_account_id: Option<&'a str>,
    payload: &'a serde_json::Value,
    outcome: &'a str,
    error_code: Option<&'a str>,
    client_ip: Option<&'a str>,
}

/// Diesel `Queryable` row shape for tests and (future) read paths.
/// Currently unused by application code; tests (T039 onward) will
/// construct it via `SELECT * FROM admin_actions`.
#[derive(Queryable, Selectable, Debug, Clone)]
#[diesel(table_name = admin_actions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
#[allow(dead_code)]
pub struct AdminActionRow {
    pub id: i64,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub operator_identity: String,
    pub action_kind: String,
    pub target_account_id: Option<String>,
    pub payload: serde_json::Value,
    pub outcome: String,
    pub error_code: Option<String>,
    pub client_ip: Option<String>,
}

/// Audit writer that INSERTs into `admin_actions`. Falls back to
/// `LogAuditor` on transient INSERT failure so audit events are never
/// silently lost (FR-027).
#[derive(Clone)]
pub struct PostgresAuditor {
    pool: Pool<AsyncPgConnection>,
    fallback: Arc<LogAuditor>,
}

impl PostgresAuditor {
    /// Build an auditor that shares the same connection pool as the
    /// rest of the Guardian metadata layer.
    pub fn new(pool: Pool<AsyncPgConnection>) -> Self {
        Self {
            pool,
            fallback: Arc::new(LogAuditor::new()),
        }
    }
}

impl PostgresAuditor {
    /// Spawn the audit-write task and return its `JoinHandle`. The
    /// production [`Auditor::record`] impl discards the handle
    /// (fire-and-forget, FR-027), but tests can `await` it to assert
    /// on the fallback path deterministically.
    pub fn record_with_handle(&self, event: AuditEvent) -> tokio::task::JoinHandle<()> {
        let pool = self.pool.clone();
        let fallback = self.fallback.clone();
        tokio::spawn(async move {
            let outcome = event.outcome.as_str();
            let result = async {
                let mut conn = pool
                    .get()
                    .await
                    .map_err(|error| format!("audit pool: {error}"))?;
                let new_row = NewAdminAction {
                    operator_identity: &event.operator_identity,
                    action_kind: event.action_kind,
                    target_account_id: event.target_account_id.as_deref(),
                    payload: &event.payload,
                    outcome,
                    error_code: event.error_code.as_deref(),
                    client_ip: event.client_ip.as_deref(),
                };
                diesel::insert_into(admin_actions::table)
                    .values(&new_row)
                    .execute(&mut conn)
                    .await
                    .map_err(|error| format!("audit insert: {error}"))?;
                Ok::<(), String>(())
            }
            .await;

            if let Err(error) = result {
                tracing::warn!(
                    target: "audit.admin_action.db_error",
                    operator_identity = %event.operator_identity,
                    action_kind = %event.action_kind,
                    outcome = %outcome,
                    error = %error,
                    "admin_actions insert failed; emitting log fallback",
                );
                fallback.record(event);
            }
        })
    }
}

impl Auditor for PostgresAuditor {
    fn record(&self, event: AuditEvent) {
        // Audit writes are fire-and-forget from the caller's
        // perspective (FR-027): the denial response must not block on
        // the row landing. The spawned task runs to completion in the
        // background regardless of whether we keep the handle; if
        // Diesel rejects (DB down, pool exhausted, schema mismatch),
        // the same event flows through `LogAuditor` inside the task
        // so the forensic record survives.
        drop(self.record_with_handle(event));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{AuditEvent, AuditOutcome, kinds};
    use crate::storage::postgres::build_postgres_pool_lazy;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tracing::Level;
    use tracing_subscriber::fmt::MakeWriter;

    // -----------------------------------------------------------------
    // Test plumbing: tracing capture so the fault-injection assertion
    // can verify that the log-fallback line is emitted under the
    // documented selector when the Postgres write fails.
    // -----------------------------------------------------------------

    #[derive(Clone, Default)]
    struct CapturedWriter {
        buf: Arc<Mutex<Vec<u8>>>,
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

    fn sample_event() -> AuditEvent {
        AuditEvent {
            operator_identity: "0xfaultinjected".into(),
            action_kind: kinds::AUTH_DENIED,
            target_account_id: None,
            payload: json!({
                "route_path": "/dashboard/_authz_probe",
                "http_method": "POST",
                "required_permissions": ["accounts:pause"],
            }),
            outcome: AuditOutcome::Denied,
            error_code: Some("GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION".into()),
            client_ip: Some("203.0.113.9".into()),
        }
    }

    /// Feature 006-operator-authz FR-027 / SC-012: when the Postgres
    /// INSERT fails (here: pool points at a non-routable address so
    /// `pool.get()` errors before any SQL is issued), the writer
    /// MUST emit the same structured log line that
    /// non-Postgres deployments produce, under
    /// `target = "audit.admin_action"`. Verifies the dual-channel
    /// behavior without needing a live Postgres.
    #[tokio::test]
    async fn postgres_write_failure_emits_log_fallback() {
        let pool = build_postgres_pool_lazy(
            "postgresql://127.0.0.1:1/__guardian_fault_injection_test__",
            1,
        )
        .expect("lazy pool builds even with bad URL");
        let auditor = PostgresAuditor::new(pool);

        // Scope the captured subscriber so concurrent tests can't
        // interfere. `with_default` activates only inside the closure.
        let writer = CapturedWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer.clone())
            .with_max_level(Level::TRACE)
            .with_ansi(false)
            .finish();

        // tokio-spawned work inherits the calling task's subscriber
        // context when we set it as the default for the duration of
        // the spawn. Use `with_default` to scope.
        tracing::subscriber::with_default(subscriber, || {
            // Spawn + await synchronously inside the scope so all
            // tracing events from the spawned task land in `writer`.
            futures::executor::block_on(async {
                let handle = auditor.record_with_handle(sample_event());
                handle.await.expect("spawned task should not panic");
            });
        });

        let captured = writer.contents();
        // The fallback path emits two events: the db_error breadcrumb
        // AND the LogAuditor fallback record under the audit selector.
        assert!(
            captured.contains("audit.admin_action.db_error"),
            "expected db_error breadcrumb in: {captured}",
        );
        assert!(
            captured.contains("audit.admin_action") && captured.contains("auth.denied"),
            "expected log-fallback audit event in: {captured}",
        );
        assert!(
            captured.contains("0xfaultinjected"),
            "expected operator_identity in fallback log: {captured}",
        );
    }

    /// T040 (FR-026 / SC-009): the Postgres append-only trigger
    /// `admin_actions_no_update` MUST block UPDATE and DELETE on a
    /// persisted row. Marked `#[ignore]` because it requires a live
    /// Postgres reachable at `DATABASE_URL` with the migration
    /// applied — run via `cargo test -p guardian-server --lib
    /// --features authz-test-probe -- --ignored postgres_trigger`.
    #[tokio::test]
    #[ignore = "requires DATABASE_URL with migrations applied"]
    async fn postgres_trigger_blocks_update_and_delete() {
        use diesel::ExpressionMethods;
        use diesel::QueryDsl;
        use diesel_async::RunQueryDsl;

        let database_url = std::env::var("DATABASE_URL")
            .expect("DATABASE_URL must be set; this test is marked #[ignore] by default");
        crate::storage::postgres::run_migrations(&database_url)
            .await
            .expect("migrations run");
        let pool = build_postgres_pool_lazy(&database_url, 1).expect("pool builds");
        // Sanity: prove the pool is reachable; if not, abort early
        // with a clear error rather than a confusing trigger failure.
        let _ = pool
            .get()
            .await
            .expect("DATABASE_URL must point at a reachable Postgres");

        let auditor = PostgresAuditor::new(pool.clone());

        // Seed one row through the production path.
        auditor
            .record_with_handle(sample_event())
            .await
            .expect("seed insert task");

        // Grab the just-inserted row id. (We can't easily extract it
        // from `record_with_handle`; use a `LIMIT 1 ORDER BY id DESC`
        // selector against the same identity.)
        let mut conn = pool.get().await.expect("conn");
        let row_id: i64 = admin_actions::table
            .filter(admin_actions::operator_identity.eq("0xfaultinjected"))
            .order(admin_actions::id.desc())
            .select(admin_actions::id)
            .first(&mut conn)
            .await
            .expect("seeded row must exist");

        // UPDATE must fail with the trigger's `RAISE EXCEPTION`.
        let update_err = diesel::update(admin_actions::table.filter(admin_actions::id.eq(row_id)))
            .set(admin_actions::outcome.eq("success"))
            .execute(&mut conn)
            .await
            .expect_err("UPDATE must be blocked");
        assert!(
            update_err
                .to_string()
                .contains("admin_actions is append-only"),
            "UPDATE error should match the trigger message: {update_err}",
        );

        // DELETE must also fail.
        let delete_err = diesel::delete(admin_actions::table.filter(admin_actions::id.eq(row_id)))
            .execute(&mut conn)
            .await
            .expect_err("DELETE must be blocked");
        assert!(
            delete_err
                .to_string()
                .contains("admin_actions is append-only"),
            "DELETE error should match the trigger message: {delete_err}",
        );
    }
}
