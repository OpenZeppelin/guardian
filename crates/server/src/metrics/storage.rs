//! Storage instrumentation decorator.
//!
//! Wraps the `Arc<dyn StorageBackend>` held by `AppState`, so every
//! caller — HTTP handlers, gRPC handlers, dashboard services, the
//! canonicalization worker — is instrumented from one place and the
//! filesystem/postgres backends stay metric-free.
//!
//! Every trait method is forwarded explicitly, including the ones with
//! default implementations (`pull_states_batch`,
//! `has_pending_candidate`, `pull_canonical_deltas_after`,
//! `pull_pending_proposals`): forwarding them preserves backend
//! overrides (e.g. the batched Postgres `pull_states_batch`), which a
//! decorator relying on the trait defaults would silently bypass.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use metrics::{counter, histogram};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use super::names::{
    LABEL_OPERATION, LABEL_OUTCOME, STORAGE_OPERATION_DURATION_SECONDS, STORAGE_OPERATIONS_TOTAL,
};
use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::state_object::StateObject;
use crate::storage::{
    AccountDeltaCursor, AccountProposalCursor, DeltaStatusCounts, DeltaStatusKind,
    GlobalDeltaCursor, GlobalDeltaRow, GlobalProposalCursor, ProposalRecord, StorageBackend,
    StorageType,
};

/// Record one storage operation: duration histogram plus an
/// `{operation, outcome}` counter. `operation` is always a static
/// method name and `outcome` is `ok`/`error` — both bounded.
async fn timed<T>(
    operation: &'static str,
    future: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let started = Instant::now();
    let result = future.await;
    let outcome = super::labels::Outcome::from_ok(result.is_ok());
    counter!(STORAGE_OPERATIONS_TOTAL,
        LABEL_OPERATION => operation, LABEL_OUTCOME => outcome.as_str())
    .increment(1);
    histogram!(STORAGE_OPERATION_DURATION_SECONDS, LABEL_OPERATION => operation)
        .record(started.elapsed().as_secs_f64());
    result
}

/// Transparent metrics decorator over any [`StorageBackend`].
pub struct InstrumentedStorage {
    inner: Arc<dyn StorageBackend>,
}

impl InstrumentedStorage {
    pub fn new(inner: Arc<dyn StorageBackend>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl StorageBackend for InstrumentedStorage {
    fn kind(&self) -> StorageType {
        self.inner.kind()
    }

    fn pool_status(&self) -> Option<crate::storage::PoolStatus> {
        self.inner.pool_status()
    }

    async fn submit_state(&self, state: &StateObject) -> Result<(), String> {
        timed("submit_state", self.inner.submit_state(state)).await
    }

    async fn submit_delta(&self, delta: &DeltaObject) -> Result<(), String> {
        timed("submit_delta", self.inner.submit_delta(delta)).await
    }

    async fn pull_state(&self, account_id: &str) -> Result<StateObject, String> {
        timed("pull_state", self.inner.pull_state(account_id)).await
    }

    async fn pull_states_batch(
        &self,
        account_ids: &[&str],
    ) -> Result<HashMap<String, StateObject>, String> {
        timed(
            "pull_states_batch",
            self.inner.pull_states_batch(account_ids),
        )
        .await
    }

    async fn pull_delta(&self, account_id: &str, nonce: u64) -> Result<DeltaObject, String> {
        timed("pull_delta", self.inner.pull_delta(account_id, nonce)).await
    }

    async fn pull_deltas_after(
        &self,
        account_id: &str,
        from_nonce: u64,
    ) -> Result<Vec<DeltaObject>, String> {
        timed(
            "pull_deltas_after",
            self.inner.pull_deltas_after(account_id, from_nonce),
        )
        .await
    }

    async fn has_pending_candidate(&self, account_id: &str) -> Result<bool, String> {
        timed(
            "has_pending_candidate",
            self.inner.has_pending_candidate(account_id),
        )
        .await
    }

    async fn pull_canonical_deltas_after(
        &self,
        account_id: &str,
        from_nonce: u64,
    ) -> Result<Vec<DeltaObject>, String> {
        timed(
            "pull_canonical_deltas_after",
            self.inner
                .pull_canonical_deltas_after(account_id, from_nonce),
        )
        .await
    }

    async fn submit_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String> {
        timed(
            "submit_delta_proposal",
            self.inner.submit_delta_proposal(commitment, proposal),
        )
        .await
    }

    async fn pull_delta_proposal(
        &self,
        account_id: &str,
        commitment: &str,
    ) -> Result<DeltaObject, String> {
        timed(
            "pull_delta_proposal",
            self.inner.pull_delta_proposal(account_id, commitment),
        )
        .await
    }

    async fn pull_all_delta_proposals(&self, account_id: &str) -> Result<Vec<DeltaObject>, String> {
        timed(
            "pull_all_delta_proposals",
            self.inner.pull_all_delta_proposals(account_id),
        )
        .await
    }

    async fn pull_pending_proposals(&self, account_id: &str) -> Result<Vec<DeltaObject>, String> {
        timed(
            "pull_pending_proposals",
            self.inner.pull_pending_proposals(account_id),
        )
        .await
    }

    async fn update_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String> {
        timed(
            "update_delta_proposal",
            self.inner.update_delta_proposal(commitment, proposal),
        )
        .await
    }

    async fn delete_delta_proposal(
        &self,
        account_id: &str,
        commitment: &str,
    ) -> Result<(), String> {
        timed(
            "delete_delta_proposal",
            self.inner.delete_delta_proposal(account_id, commitment),
        )
        .await
    }

    async fn delete_delta(&self, account_id: &str, nonce: u64) -> Result<(), String> {
        timed("delete_delta", self.inner.delete_delta(account_id, nonce)).await
    }

    async fn update_delta_status(
        &self,
        account_id: &str,
        nonce: u64,
        status: DeltaStatus,
    ) -> Result<(), String> {
        timed(
            "update_delta_status",
            self.inner.update_delta_status(account_id, nonce, status),
        )
        .await
    }

    async fn list_account_deltas_paged(
        &self,
        account_id: &str,
        limit: u32,
        cursor: Option<AccountDeltaCursor>,
    ) -> Result<Vec<DeltaObject>, String> {
        timed(
            "list_account_deltas_paged",
            self.inner
                .list_account_deltas_paged(account_id, limit, cursor),
        )
        .await
    }

    async fn list_account_proposals_paged(
        &self,
        account_id: &str,
        limit: u32,
        cursor: Option<AccountProposalCursor>,
    ) -> Result<Vec<ProposalRecord>, String> {
        timed(
            "list_account_proposals_paged",
            self.inner
                .list_account_proposals_paged(account_id, limit, cursor),
        )
        .await
    }

    async fn list_global_deltas_paged(
        &self,
        limit: u32,
        cursor: Option<GlobalDeltaCursor>,
        status_filter: Option<Vec<DeltaStatusKind>>,
    ) -> Result<Vec<GlobalDeltaRow>, String> {
        timed(
            "list_global_deltas_paged",
            self.inner
                .list_global_deltas_paged(limit, cursor, status_filter),
        )
        .await
    }

    async fn list_global_proposals_paged(
        &self,
        limit: u32,
        cursor: Option<GlobalProposalCursor>,
    ) -> Result<Vec<ProposalRecord>, String> {
        timed(
            "list_global_proposals_paged",
            self.inner.list_global_proposals_paged(limit, cursor),
        )
        .await
    }

    async fn count_deltas_by_status(&self) -> Result<DeltaStatusCounts, String> {
        timed(
            "count_deltas_by_status",
            self.inner.count_deltas_by_status(),
        )
        .await
    }

    async fn count_in_flight_proposals(&self) -> Result<u64, String> {
        timed(
            "count_in_flight_proposals",
            self.inner.count_in_flight_proposals(),
        )
        .await
    }

    async fn latest_activity_timestamp(&self) -> Result<Option<DateTime<Utc>>, String> {
        timed(
            "latest_activity_timestamp",
            self.inner.latest_activity_timestamp(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::recorder::build_recorder;
    use crate::testing::mocks::MockStorageBackend;

    fn render_after(run: impl FnOnce(Arc<dyn StorageBackend>)) -> String {
        let recorder = build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            run(Arc::new(InstrumentedStorage::new(Arc::new(
                MockStorageBackend::new(),
            ))));
        });
        handle.render()
    }

    fn block_on<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn records_ok_outcome_and_duration() {
        let rendered = render_after(|storage| {
            block_on(async move {
                storage.count_in_flight_proposals().await.unwrap();
            });
        });
        assert!(
            rendered.contains(
                "guardian_storage_operations_total{operation=\"count_in_flight_proposals\",\
                 outcome=\"ok\"} 1"
            ),
            "missing ok counter in:\n{rendered}"
        );
        assert!(rendered.contains(
            "guardian_storage_operation_duration_seconds_bucket{\
             operation=\"count_in_flight_proposals\""
        ));
    }

    #[test]
    fn records_error_outcome() {
        let rendered = render_after(|storage| {
            block_on(async move {
                // Mock pull_state queue is empty → defaults to Err.
                let _ = storage.pull_state("acct").await;
            });
        });
        assert!(
            rendered
                .contains("guardian_storage_operations_total{operation=\"pull_state\",outcome="),
            "missing pull_state counter in:\n{rendered}"
        );
        assert!(
            rendered.contains("outcome=\"error\"} 1"),
            "missing error outcome in:\n{rendered}"
        );
    }

    #[test]
    fn kind_forwards_without_metrics() {
        let recorder = build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            let storage = InstrumentedStorage::new(Arc::new(MockStorageBackend::new()));
            let _ = storage.kind();
        });
        assert!(
            !handle
                .render()
                .contains("guardian_storage_operations_total")
        );
    }
}
