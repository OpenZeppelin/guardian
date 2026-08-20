use crate::delta_object::DeltaObject;
use crate::metadata::MetadataStore;
use crate::metadata::auth::{Auth, Credentials};
use crate::network::{NetworkClient, StateVerification};
use crate::state_object::StateObject;
use crate::storage::StorageBackend;
use async_trait::async_trait;
use guardian_shared::FromJson;
use miden_protocol::account::Account;
use std::sync::{Arc, Mutex as StdMutex};

type StdResult<T, E> = std::result::Result<T, E>;
type ApplyDeltaResult = StdResult<(serde_json::Value, String), String>;
type ShouldUpdateAuthResult = StdResult<Option<Auth>, String>;
type ExtractGuardianCommitmentResult = StdResult<Option<String>, String>;
type PullDeltasResult = StdResult<Vec<DeltaObject>, String>;
type GetMetadataResult = StdResult<Option<crate::metadata::AccountMetadata>, String>;
type ListResult = StdResult<Vec<String>, String>;

fn delta_to_proposal_record(proposal: DeltaObject) -> crate::storage::ProposalRecord {
    crate::storage::ProposalRecord {
        account_id: proposal.account_id.clone(),
        commitment: proposal.new_commitment.clone().unwrap_or_default(),
        proposal,
    }
}

#[derive(Clone, Default)]
pub struct MockNetworkClient {
    pub verify_commitment_responses: Arc<StdMutex<Vec<StdResult<StateVerification, String>>>>,
    pub verify_commitment_calls: Arc<StdMutex<Vec<(String, String)>>>,
    pub verify_commitment_modes: Arc<StdMutex<Vec<crate::network::RpcReadMode>>>,
    pub get_state_commitment_responses: Arc<StdMutex<Vec<StdResult<String, String>>>>,
    pub get_state_commitment_calls: Arc<StdMutex<Vec<(String, serde_json::Value)>>>,
    pub validate_credential_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub validate_guardian_commitment_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub verify_delta_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub apply_delta_responses: Arc<StdMutex<Vec<ApplyDeltaResult>>>,
    pub should_update_auth_responses: Arc<StdMutex<Vec<ShouldUpdateAuthResult>>>,
    pub extract_guardian_commitment_responses: Arc<StdMutex<Vec<ExtractGuardianCommitmentResult>>>,
}

impl MockNetworkClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_verify_commitment(self, response: StdResult<StateVerification, String>) -> Self {
        self.verify_commitment_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_get_state_commitment(self, response: StdResult<String, String>) -> Self {
        self.get_state_commitment_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_validate_credential(self, response: StdResult<(), String>) -> Self {
        self.validate_credential_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_validate_guardian_commitment(self, response: StdResult<(), String>) -> Self {
        self.validate_guardian_commitment_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_verify_delta(self, response: StdResult<(), String>) -> Self {
        self.verify_delta_responses.lock().unwrap().push(response);
        self
    }

    pub fn with_extract_guardian_commitment(
        self,
        response: StdResult<Option<String>, String>,
    ) -> Self {
        self.extract_guardian_commitment_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_apply_delta(
        self,
        response: StdResult<(serde_json::Value, String), String>,
    ) -> Self {
        self.apply_delta_responses.lock().unwrap().push(response);
        self
    }

    pub fn with_should_update_auth(self, response: StdResult<Option<Auth>, String>) -> Self {
        self.should_update_auth_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn get_verify_commitment_calls(&self) -> Vec<(String, String)> {
        self.verify_commitment_calls.lock().unwrap().clone()
    }

    pub fn get_verify_commitment_modes(&self) -> Vec<crate::network::RpcReadMode> {
        self.verify_commitment_modes.lock().unwrap().clone()
    }

    pub fn get_state_commitment_calls(&self) -> Vec<(String, serde_json::Value)> {
        self.get_state_commitment_calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl NetworkClient for MockNetworkClient {
    fn get_state_commitment(
        &self,
        account_id: &str,
        state_json: &serde_json::Value,
    ) -> StdResult<String, String> {
        self.get_state_commitment_calls
            .lock()
            .unwrap()
            .push((account_id.to_string(), state_json.clone()));

        if let Some(response) = self.get_state_commitment_responses.lock().unwrap().pop() {
            return response;
        }

        let account = Account::from_json(state_json)
            .map_err(|e| format!("Failed to deserialize account: {e}"))?;
        let commitment_hex = format!("0x{}", hex::encode(account.to_commitment().as_bytes()));
        Ok(commitment_hex)
    }

    async fn verify_commitment(
        &self,
        account_id: &str,
        expected_commitment: &str,
        read_mode: crate::network::RpcReadMode,
    ) -> StdResult<StateVerification, String> {
        self.verify_commitment_calls
            .lock()
            .unwrap()
            .push((account_id.to_string(), expected_commitment.to_string()));
        self.verify_commitment_modes.lock().unwrap().push(read_mode);

        self.verify_commitment_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(StateVerification::Match))
    }

    fn verify_delta(
        &self,
        _prev_proof: &str,
        _prev_state_json: &serde_json::Value,
        _delta_payload: &serde_json::Value,
    ) -> StdResult<(), String> {
        self.verify_delta_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(()))
    }

    fn apply_delta(
        &self,
        _prev_state_json: &serde_json::Value,
        _delta_payload: &serde_json::Value,
    ) -> StdResult<(serde_json::Value, String), String> {
        self.apply_delta_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok((serde_json::json!({}), "mock_new_commitment".to_string())))
    }

    fn merge_deltas(
        &self,
        _delta_payloads: Vec<serde_json::Value>,
    ) -> StdResult<serde_json::Value, String> {
        Ok(serde_json::json!({}))
    }

    fn validate_account_id(&self, _account_id: &str) -> StdResult<(), String> {
        Ok(())
    }

    fn validate_credential(
        &self,
        _state_json: &serde_json::Value,
        _credential: &Credentials,
        _auth: &Auth,
    ) -> StdResult<(), String> {
        self.validate_credential_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(()))
    }

    fn validate_guardian_commitment(
        &self,
        _state_json: &serde_json::Value,
        _expected_guardian_commitment: &str,
    ) -> StdResult<(), String> {
        self.validate_guardian_commitment_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(()))
    }

    fn extract_guardian_commitment(
        &self,
        _state_json: &serde_json::Value,
    ) -> StdResult<Option<String>, String> {
        // Default `Ok(None)` ("no guardian binding visible") keeps the
        // release-on-switch hook inert in tests that don't opt in.
        self.extract_guardian_commitment_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(None))
    }

    async fn should_update_auth(
        &self,
        _state_json: &serde_json::Value,
        _current_auth: &Auth,
    ) -> StdResult<Option<Auth>, String> {
        self.should_update_auth_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(None))
    }

    fn delta_proposal_id(
        &self,
        _account_id: &str,
        _nonce: u64,
        _delta_payload: &serde_json::Value,
    ) -> Result<String, String> {
        Ok(format!("0x{}", "ab".repeat(32)))
    }
}

#[derive(Clone, Default)]
#[allow(clippy::type_complexity)]
pub struct MockStorageBackend {
    /// Reported by [`StorageBackend::kind`]. Defaults to
    /// [`StorageType::Postgres`] (no FR-029 threshold). Tests
    /// asserting filesystem-degraded behavior set this to
    /// `Filesystem` via [`Self::with_kind`].
    pub kind: Option<crate::storage::StorageType>,
    /// Reported by [`StorageBackend::pool_status`]. Defaults to `None`
    /// (no pool); set via [`Self::with_pool_status`].
    pub pool_status: Option<crate::storage::PoolStatus>,
    pub submit_state_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub submit_state_calls: Arc<StdMutex<Vec<StateObject>>>,
    pub submit_delta_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub submit_delta_calls: Arc<StdMutex<Vec<DeltaObject>>>,
    pub pull_state_responses: Arc<StdMutex<Vec<StdResult<StateObject, String>>>>,
    pub pull_delta_responses: Arc<StdMutex<Vec<StdResult<DeltaObject, String>>>>,
    pub pull_deltas_after_responses: Arc<StdMutex<Vec<PullDeltasResult>>>,
    pub pull_candidate_deltas_responses: Arc<StdMutex<Vec<PullDeltasResult>>>,
    pub pull_recent_candidate_deltas_responses: Arc<StdMutex<Vec<PullDeltasResult>>>,
    pub pull_recent_candidate_deltas_calls: Arc<
        StdMutex<
            Vec<(
                chrono::DateTime<chrono::Utc>,
                Option<crate::storage::RecentCandidateCursor>,
                u32,
            )>,
        >,
    >,
    pub pull_recoverable_deltas_responses: Arc<StdMutex<Vec<PullDeltasResult>>>,
    pub list_accounts_with_recoverable_deltas_responses:
        Arc<StdMutex<Vec<StdResult<Vec<String>, String>>>>,
    pub submit_delta_proposal_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub submit_delta_proposal_calls: Arc<StdMutex<Vec<(String, DeltaObject)>>>,
    pub pull_delta_proposal_responses: Arc<StdMutex<Vec<StdResult<DeltaObject, String>>>>,
    pub pull_delta_proposal_calls: Arc<StdMutex<Vec<(String, String)>>>,
    pub pull_all_delta_proposals_responses: Arc<StdMutex<Vec<StdResult<Vec<DeltaObject>, String>>>>,
    pub pull_all_delta_proposals_calls: Arc<StdMutex<Vec<String>>>,
    pub update_delta_proposal_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub update_delta_proposal_calls: Arc<StdMutex<Vec<(String, DeltaObject)>>>,
    pub delete_delta_proposal_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub delete_delta_proposal_calls: Arc<StdMutex<Vec<(String, String)>>>,
    pub delete_delta_calls: Arc<StdMutex<Vec<(String, u64)>>>,
    pub request_candidate_abandon_responses:
        Arc<StdMutex<Vec<StdResult<crate::storage::AbandonIntent, String>>>>,
    pub update_delta_status_calls:
        Arc<StdMutex<Vec<(String, u64, crate::delta_object::DeltaStatus)>>>,
    // Canonicalization lifecycle writes. When a scripted outcome is
    // queued it is returned directly (the fenced-backend behaviors:
    // `StaleLease` / `NotCandidate` / an error); otherwise the call
    // falls through to the single-process sequential helper so
    // existing tests keep observing the underlying submit/delete/
    // update calls.
    pub submit_candidate_responses:
        Arc<StdMutex<Vec<StdResult<crate::storage::CandidateSubmission, String>>>>,
    pub promote_candidate_responses:
        Arc<StdMutex<Vec<StdResult<crate::storage::PromoteWrite, String>>>>,
    pub promote_candidate_fences: Arc<StdMutex<Vec<Option<crate::storage::LeaseFence>>>>,
    pub discard_candidate_responses:
        Arc<StdMutex<Vec<StdResult<crate::storage::CanonicalWrite, String>>>>,
    pub discard_candidate_calls: Arc<StdMutex<Vec<(String, u64, crate::storage::DeltaStatusKind)>>>,
    pub update_candidate_status_responses:
        Arc<StdMutex<Vec<StdResult<crate::storage::CanonicalWrite, String>>>>,
    // Dashboard read APIs (feature `005-operator-dashboard-metrics`).
    // Each queue is consumed LIFO via `Vec::pop`, mirroring the
    // existing helpers — callers either push N identical responses or
    // push them in reverse order to control per-call values.
    pub list_account_deltas_paged_responses:
        Arc<StdMutex<Vec<StdResult<Vec<DeltaObject>, String>>>>,
    pub list_canonical_deltas_paged_responses:
        Arc<StdMutex<Vec<StdResult<Vec<DeltaObject>, String>>>>,
    pub list_account_proposals_paged_responses:
        Arc<StdMutex<Vec<StdResult<Vec<crate::storage::ProposalRecord>, String>>>>,
    pub list_global_deltas_paged_responses:
        Arc<StdMutex<Vec<StdResult<Vec<crate::storage::GlobalDeltaRow>, String>>>>,
    pub list_global_proposals_paged_responses:
        Arc<StdMutex<Vec<StdResult<Vec<crate::storage::ProposalRecord>, String>>>>,
    pub count_deltas_by_status_responses:
        Arc<StdMutex<Vec<StdResult<crate::storage::DeltaStatusCounts, String>>>>,
    /// Number of `count_deltas_by_status` invocations; the metrics
    /// refresher tests assert ticking cadence against this.
    pub count_deltas_by_status_calls: Arc<StdMutex<u64>>,
    pub count_in_flight_proposals_responses: Arc<StdMutex<Vec<StdResult<u64, String>>>>,
    pub latest_activity_timestamp_responses:
        Arc<StdMutex<Vec<StdResult<Option<chrono::DateTime<chrono::Utc>>, String>>>>,
}

impl MockStorageBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_kind(mut self, kind: crate::storage::StorageType) -> Self {
        self.kind = Some(kind);
        self
    }

    pub fn with_pool_status(mut self, status: crate::storage::PoolStatus) -> Self {
        self.pool_status = Some(status);
        self
    }

    pub fn with_submit_state(self, response: StdResult<(), String>) -> Self {
        self.submit_state_responses.lock().unwrap().push(response);
        self
    }

    pub fn with_submit_delta(self, response: StdResult<(), String>) -> Self {
        self.submit_delta_responses.lock().unwrap().push(response);
        self
    }

    pub fn with_pull_state(self, response: StdResult<StateObject, String>) -> Self {
        self.pull_state_responses.lock().unwrap().push(response);
        self
    }

    pub fn with_pull_delta(self, response: StdResult<DeltaObject, String>) -> Self {
        self.pull_delta_responses.lock().unwrap().push(response);
        self
    }

    pub fn with_pull_deltas_after(self, response: StdResult<Vec<DeltaObject>, String>) -> Self {
        self.pull_deltas_after_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_pull_candidate_deltas(self, response: StdResult<Vec<DeltaObject>, String>) -> Self {
        self.pull_candidate_deltas_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_pull_recent_candidate_deltas(
        self,
        response: StdResult<Vec<DeltaObject>, String>,
    ) -> Self {
        self.pull_recent_candidate_deltas_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_pull_recoverable_deltas(
        self,
        response: StdResult<Vec<DeltaObject>, String>,
    ) -> Self {
        self.pull_recoverable_deltas_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_list_accounts_with_recoverable_deltas(
        self,
        response: StdResult<Vec<String>, String>,
    ) -> Self {
        self.list_accounts_with_recoverable_deltas_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn get_pull_recent_candidate_deltas_calls(
        &self,
    ) -> Vec<(
        chrono::DateTime<chrono::Utc>,
        Option<crate::storage::RecentCandidateCursor>,
        u32,
    )> {
        self.pull_recent_candidate_deltas_calls
            .lock()
            .unwrap()
            .clone()
    }

    pub fn get_discard_candidate_calls(
        &self,
    ) -> Vec<(String, u64, crate::storage::DeltaStatusKind)> {
        self.discard_candidate_calls.lock().unwrap().clone()
    }

    pub fn get_submit_state_calls(&self) -> Vec<StateObject> {
        self.submit_state_calls.lock().unwrap().clone()
    }

    pub fn get_submit_delta_calls(&self) -> Vec<DeltaObject> {
        self.submit_delta_calls.lock().unwrap().clone()
    }

    pub fn with_submit_delta_proposal(self, response: StdResult<(), String>) -> Self {
        self.submit_delta_proposal_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_pull_delta_proposal(self, response: StdResult<DeltaObject, String>) -> Self {
        self.pull_delta_proposal_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_pull_all_delta_proposals(
        self,
        response: StdResult<Vec<DeltaObject>, String>,
    ) -> Self {
        self.pull_all_delta_proposals_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_update_delta_proposal(self, response: StdResult<(), String>) -> Self {
        self.update_delta_proposal_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_delete_delta_proposal(self, response: StdResult<(), String>) -> Self {
        self.delete_delta_proposal_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn get_submit_delta_proposal_calls(&self) -> Vec<(String, DeltaObject)> {
        self.submit_delta_proposal_calls.lock().unwrap().clone()
    }

    pub fn get_pull_delta_proposal_calls(&self) -> Vec<(String, String)> {
        self.pull_delta_proposal_calls.lock().unwrap().clone()
    }

    pub fn get_pull_all_delta_proposals_calls(&self) -> Vec<String> {
        self.pull_all_delta_proposals_calls.lock().unwrap().clone()
    }

    pub fn get_update_delta_proposal_calls(&self) -> Vec<(String, DeltaObject)> {
        self.update_delta_proposal_calls.lock().unwrap().clone()
    }

    pub fn get_delete_delta_proposal_calls(&self) -> Vec<(String, String)> {
        self.delete_delta_proposal_calls.lock().unwrap().clone()
    }

    pub fn with_request_candidate_abandon(
        self,
        response: StdResult<crate::storage::AbandonIntent, String>,
    ) -> Self {
        self.request_candidate_abandon_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn get_delete_delta_calls(&self) -> Vec<(String, u64)> {
        self.delete_delta_calls.lock().unwrap().clone()
    }

    pub fn get_update_delta_status_calls(
        &self,
    ) -> Vec<(String, u64, crate::delta_object::DeltaStatus)> {
        self.update_delta_status_calls.lock().unwrap().clone()
    }

    pub fn with_submit_candidate(
        self,
        response: StdResult<crate::storage::CandidateSubmission, String>,
    ) -> Self {
        self.submit_candidate_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_promote_candidate(
        self,
        response: StdResult<crate::storage::PromoteWrite, String>,
    ) -> Self {
        self.promote_candidate_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_discard_candidate(
        self,
        response: StdResult<crate::storage::CanonicalWrite, String>,
    ) -> Self {
        self.discard_candidate_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_update_candidate_status(
        self,
        response: StdResult<crate::storage::CanonicalWrite, String>,
    ) -> Self {
        self.update_candidate_status_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn get_promote_candidate_fences(&self) -> Vec<Option<crate::storage::LeaseFence>> {
        self.promote_candidate_fences.lock().unwrap().clone()
    }

    // Dashboard read APIs (feature `005-operator-dashboard-metrics`).

    pub fn with_list_account_deltas_paged(
        self,
        response: StdResult<Vec<DeltaObject>, String>,
    ) -> Self {
        self.list_account_deltas_paged_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_list_canonical_deltas_paged(
        self,
        response: StdResult<Vec<DeltaObject>, String>,
    ) -> Self {
        self.list_canonical_deltas_paged_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_list_account_proposals_paged(
        self,
        response: StdResult<Vec<crate::storage::ProposalRecord>, String>,
    ) -> Self {
        self.list_account_proposals_paged_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_list_global_deltas_paged(
        self,
        response: StdResult<Vec<crate::storage::GlobalDeltaRow>, String>,
    ) -> Self {
        self.list_global_deltas_paged_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_list_global_proposals_paged(
        self,
        response: StdResult<Vec<crate::storage::ProposalRecord>, String>,
    ) -> Self {
        self.list_global_proposals_paged_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_count_deltas_by_status(
        self,
        response: StdResult<crate::storage::DeltaStatusCounts, String>,
    ) -> Self {
        self.count_deltas_by_status_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_count_in_flight_proposals(self, response: StdResult<u64, String>) -> Self {
        self.count_in_flight_proposals_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_latest_activity_timestamp(
        self,
        response: StdResult<Option<chrono::DateTime<chrono::Utc>>, String>,
    ) -> Self {
        self.latest_activity_timestamp_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }
}

#[async_trait]
impl StorageBackend for MockStorageBackend {
    fn kind(&self) -> crate::storage::StorageType {
        self.kind
            .clone()
            .unwrap_or(crate::storage::StorageType::Postgres)
    }

    fn pool_status(&self) -> Option<crate::storage::PoolStatus> {
        self.pool_status
    }

    async fn submit_state(&self, state: &StateObject) -> StdResult<(), String> {
        self.submit_state_calls.lock().unwrap().push(state.clone());
        self.submit_state_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(()))
    }

    async fn submit_delta(&self, delta: &DeltaObject) -> StdResult<(), String> {
        self.submit_delta_calls.lock().unwrap().push(delta.clone());
        self.submit_delta_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(()))
    }

    async fn pull_state(&self, _account_id: &str) -> StdResult<StateObject, String> {
        self.pull_state_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Err("No state found".to_string()))
    }

    async fn pull_delta(&self, _account_id: &str, _nonce: u64) -> StdResult<DeltaObject, String> {
        self.pull_delta_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Err("Mock: delta not found".to_string()))
    }

    async fn pull_deltas_after(
        &self,
        _account_id: &str,
        _from_nonce: u64,
    ) -> StdResult<Vec<DeltaObject>, String> {
        self.pull_deltas_after_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(vec![]))
    }

    // An explicit response wins; otherwise mirror the trait default over
    // the `pull_deltas_after` queue so existing tests keep driving the
    // processor through `with_pull_deltas_after`.
    async fn pull_candidate_deltas(&self, account_id: &str) -> StdResult<Vec<DeltaObject>, String> {
        if let Some(response) = self.pull_candidate_deltas_responses.lock().unwrap().pop() {
            return response;
        }
        let mut deltas: Vec<DeltaObject> = self
            .pull_deltas_after(account_id, 0)
            .await?
            .into_iter()
            .filter(|delta| delta.status.is_candidate())
            .collect();
        deltas.sort_by_key(|delta| delta.nonce);
        Ok(deltas)
    }

    async fn pull_recent_candidate_deltas(
        &self,
        since: chrono::DateTime<chrono::Utc>,
        cursor: Option<&crate::storage::RecentCandidateCursor>,
        limit: u32,
    ) -> StdResult<Vec<DeltaObject>, String> {
        self.pull_recent_candidate_deltas_calls
            .lock()
            .unwrap()
            .push((since, cursor.cloned(), limit));
        self.pull_recent_candidate_deltas_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    // An explicit response wins; otherwise no retained rows, so tests
    // that never touch issue #345 recovery see no behavior change.
    async fn pull_recoverable_deltas(
        &self,
        _account_id: &str,
        _abandoned_since: chrono::DateTime<chrono::Utc>,
    ) -> StdResult<Vec<DeltaObject>, String> {
        self.pull_recoverable_deltas_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(vec![]))
    }

    async fn list_accounts_with_recoverable_deltas(
        &self,
        _abandoned_since: chrono::DateTime<chrono::Utc>,
    ) -> StdResult<Vec<String>, String> {
        self.list_accounts_with_recoverable_deltas_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(vec![]))
    }

    async fn submit_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String> {
        self.submit_delta_proposal_calls
            .lock()
            .unwrap()
            .push((commitment.to_string(), proposal.clone()));
        self.submit_delta_proposal_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(()))
    }

    async fn pull_delta_proposal(
        &self,
        account_id: &str,
        commitment: &str,
    ) -> Result<DeltaObject, String> {
        self.pull_delta_proposal_calls
            .lock()
            .unwrap()
            .push((account_id.to_string(), commitment.to_string()));
        self.pull_delta_proposal_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Err("Mock: proposal not found".to_string()))
    }

    async fn pull_all_delta_proposals(
        &self,
        account_id: &str,
    ) -> Result<Vec<crate::storage::ProposalRecord>, String> {
        self.pull_all_delta_proposals_calls
            .lock()
            .unwrap()
            .push(account_id.to_string());
        self.pull_all_delta_proposals_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(vec![]))
            .map(|proposals| {
                proposals
                    .into_iter()
                    .map(delta_to_proposal_record)
                    .collect()
            })
    }

    async fn update_delta_proposal(
        &self,
        commitment: &str,
        proposal: &DeltaObject,
    ) -> Result<(), String> {
        self.update_delta_proposal_calls
            .lock()
            .unwrap()
            .push((commitment.to_string(), proposal.clone()));
        self.update_delta_proposal_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(()))
    }

    async fn delete_delta_proposal(
        &self,
        account_id: &str,
        commitment: &str,
    ) -> Result<(), String> {
        self.delete_delta_proposal_calls
            .lock()
            .unwrap()
            .push((account_id.to_string(), commitment.to_string()));
        self.delete_delta_proposal_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(()))
    }

    async fn delete_delta(&self, account_id: &str, nonce: u64) -> Result<(), String> {
        self.delete_delta_calls
            .lock()
            .unwrap()
            .push((account_id.to_string(), nonce));
        Ok(())
    }

    async fn request_candidate_abandon(
        &self,
        _account_id: &str,
        _nonce: u64,
        _now: &str,
    ) -> Result<crate::storage::AbandonIntent, String> {
        self.request_candidate_abandon_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(crate::storage::AbandonIntent::Recorded))
    }

    async fn update_delta_status(
        &self,
        account_id: &str,
        nonce: u64,
        status: crate::delta_object::DeltaStatus,
    ) -> Result<(), String> {
        self.update_delta_status_calls.lock().unwrap().push((
            account_id.to_string(),
            nonce,
            status,
        ));
        Ok(())
    }

    async fn submit_candidate(
        &self,
        metadata: &dyn crate::metadata::MetadataStore,
        delta: &DeltaObject,
        now: &str,
    ) -> Result<crate::storage::CandidateSubmission, String> {
        if let Some(response) = self.submit_candidate_responses.lock().unwrap().pop() {
            return response;
        }
        crate::storage::submit_candidate_sequential(self, metadata, delta, now).await
    }

    async fn promote_candidate(
        &self,
        metadata: &dyn crate::metadata::MetadataStore,
        promotion: crate::storage::CandidatePromotion,
    ) -> Result<crate::storage::PromoteWrite, String> {
        self.promote_candidate_fences
            .lock()
            .unwrap()
            .push(promotion.fence.clone());
        if let Some(response) = self.promote_candidate_responses.lock().unwrap().pop() {
            return response;
        }
        crate::storage::promote_candidate_sequential(self, metadata, promotion).await
    }

    async fn discard_candidate(
        &self,
        metadata: &dyn crate::metadata::MetadataStore,
        account_id: &str,
        nonce: u64,
        kind: crate::storage::DeltaStatusKind,
        now: &str,
        _fence: Option<&crate::storage::LeaseFence>,
    ) -> Result<crate::storage::CanonicalWrite, String> {
        self.discard_candidate_calls
            .lock()
            .unwrap()
            .push((account_id.to_string(), nonce, kind));
        if let Some(response) = self.discard_candidate_responses.lock().unwrap().pop() {
            return response;
        }
        crate::storage::discard_candidate_sequential(self, metadata, account_id, nonce, kind, now)
            .await
    }

    async fn update_candidate_status(
        &self,
        account_id: &str,
        nonce: u64,
        status: crate::delta_object::DeltaStatus,
        _fence: Option<&crate::storage::LeaseFence>,
    ) -> Result<crate::storage::CanonicalWrite, String> {
        if let Some(response) = self.update_candidate_status_responses.lock().unwrap().pop() {
            return response;
        }
        crate::storage::update_candidate_status_sequential(self, account_id, nonce, status).await
    }

    // Dashboard read APIs (feature `005-operator-dashboard-metrics`).

    async fn list_account_deltas_paged(
        &self,
        _account_id: &str,
        _limit: u32,
        _cursor: Option<crate::storage::AccountDeltaCursor>,
    ) -> Result<Vec<DeltaObject>, String> {
        self.list_account_deltas_paged_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    async fn list_canonical_deltas_paged(
        &self,
        _account_id: &str,
        _limit: u32,
        _cursor: Option<crate::storage::AccountDeltaCursor>,
    ) -> Result<Vec<DeltaObject>, String> {
        self.list_canonical_deltas_paged_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    async fn list_account_proposals_paged(
        &self,
        _account_id: &str,
        _limit: u32,
        _cursor: Option<crate::storage::AccountProposalCursor>,
    ) -> Result<Vec<crate::storage::ProposalRecord>, String> {
        self.list_account_proposals_paged_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    async fn list_global_deltas_paged(
        &self,
        _limit: u32,
        _cursor: Option<crate::storage::GlobalDeltaCursor>,
        _status_filter: Option<Vec<crate::storage::DeltaStatusKind>>,
    ) -> Result<Vec<crate::storage::GlobalDeltaRow>, String> {
        self.list_global_deltas_paged_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    async fn list_global_proposals_paged(
        &self,
        _limit: u32,
        _cursor: Option<crate::storage::GlobalProposalCursor>,
    ) -> Result<Vec<crate::storage::ProposalRecord>, String> {
        self.list_global_proposals_paged_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    async fn count_deltas_by_status(&self) -> Result<crate::storage::DeltaStatusCounts, String> {
        *self.count_deltas_by_status_calls.lock().unwrap() += 1;
        self.count_deltas_by_status_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(crate::storage::DeltaStatusCounts::default()))
    }

    async fn count_in_flight_proposals(&self) -> Result<u64, String> {
        self.count_in_flight_proposals_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(0))
    }

    async fn latest_activity_timestamp(
        &self,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, String> {
        self.latest_activity_timestamp_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(None))
    }
}

#[derive(Clone, Default)]
#[allow(clippy::type_complexity)]
pub struct MockMetadataStore {
    pub get_responses: Arc<StdMutex<Vec<GetMetadataResult>>>,
    pub get_calls: Arc<StdMutex<Vec<String>>>,
    pub set_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub set_calls: Arc<StdMutex<Vec<crate::metadata::AccountMetadata>>>,
    pub list_responses: Arc<StdMutex<Vec<ListResult>>>,
    pub list_paged_responses:
        Arc<StdMutex<Vec<StdResult<Vec<crate::metadata::AccountMetadata>, String>>>>,
    pub list_with_pending_candidates_responses: Arc<StdMutex<Vec<ListResult>>>,
    pub update_timestamp_cas_responses: Arc<StdMutex<Vec<StdResult<bool, String>>>>,
    pub find_by_cosigner_commitment_responses: Arc<StdMutex<Vec<ListResult>>>,
    pub find_by_cosigner_commitment_calls: Arc<StdMutex<Vec<String>>>,
    pub set_released_calls: Arc<StdMutex<Vec<String>>>,
    pub clear_released_calls: Arc<StdMutex<Vec<String>>>,
    pub clear_released_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    /// Reported by [`MetadataStore::pool_status`]. Defaults to `None`;
    /// set via [`Self::with_pool_status`].
    pub pool_status: Option<crate::storage::PoolStatus>,
}

impl MockMetadataStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_get(
        self,
        response: StdResult<Option<crate::metadata::AccountMetadata>, String>,
    ) -> Self {
        self.get_responses.lock().unwrap().push(response);
        self
    }

    pub fn with_set(self, response: StdResult<(), String>) -> Self {
        self.set_responses.lock().unwrap().push(response);
        self
    }

    pub fn with_list(self, response: StdResult<Vec<String>, String>) -> Self {
        self.list_responses.lock().unwrap().push(response);
        self
    }

    pub fn with_pool_status(mut self, status: crate::storage::PoolStatus) -> Self {
        self.pool_status = Some(status);
        self
    }

    pub fn with_list_paged(
        self,
        response: StdResult<Vec<crate::metadata::AccountMetadata>, String>,
    ) -> Self {
        self.list_paged_responses.lock().unwrap().push(response);
        self
    }

    pub fn with_list_with_pending_candidates(
        self,
        response: StdResult<Vec<String>, String>,
    ) -> Self {
        self.list_with_pending_candidates_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_update_timestamp_cas(self, response: StdResult<bool, String>) -> Self {
        self.update_timestamp_cas_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn with_clear_released(self, response: StdResult<(), String>) -> Self {
        self.clear_released_responses.lock().unwrap().push(response);
        self
    }

    pub fn with_find_by_cosigner_commitment(
        self,
        response: StdResult<Vec<String>, String>,
    ) -> Self {
        self.find_by_cosigner_commitment_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn get_find_by_cosigner_commitment_calls(&self) -> Vec<String> {
        self.find_by_cosigner_commitment_calls
            .lock()
            .unwrap()
            .clone()
    }

    pub fn get_get_calls(&self) -> Vec<String> {
        self.get_calls.lock().unwrap().clone()
    }

    pub fn get_set_calls(&self) -> Vec<crate::metadata::AccountMetadata> {
        self.set_calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl MetadataStore for MockMetadataStore {
    fn pool_status(&self) -> Option<crate::storage::PoolStatus> {
        self.pool_status
    }

    async fn get(
        &self,
        account_id: &str,
    ) -> StdResult<Option<crate::metadata::AccountMetadata>, String> {
        self.get_calls.lock().unwrap().push(account_id.to_string());
        let mut responses = self.get_responses.lock().unwrap();
        // Return cloned last response if multiple calls expected, otherwise pop
        if responses.len() > 1 {
            responses.pop().unwrap_or(Ok(None))
        } else {
            // Clone the last response to allow multiple gets without consuming
            responses.last().cloned().unwrap_or(Ok(None))
        }
    }

    async fn set(&self, metadata: crate::metadata::AccountMetadata) -> StdResult<(), String> {
        self.set_calls.lock().unwrap().push(metadata);
        // Always allow set operations by default
        self.set_responses.lock().unwrap().pop().unwrap_or(Ok(()))
    }

    async fn list(&self) -> StdResult<Vec<String>, String> {
        self.list_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(vec![]))
    }

    async fn list_paged(
        &self,
        _limit: u32,
        _cursor: Option<crate::metadata::AccountListCursor>,
        _paused: Option<bool>,
    ) -> StdResult<Vec<crate::metadata::AccountMetadata>, String> {
        self.list_paged_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    async fn list_with_pending_candidates(&self) -> StdResult<Vec<String>, String> {
        self.list_with_pending_candidates_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(vec![]))
    }

    async fn update_last_auth_timestamp_cas(
        &self,
        _account_id: &str,
        _new_timestamp: i64,
    ) -> StdResult<bool, String> {
        self.update_timestamp_cas_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(true)) // Default to success
    }

    async fn find_by_cosigner_commitment(
        &self,
        commitment: &str,
    ) -> StdResult<Vec<String>, String> {
        self.find_by_cosigner_commitment_calls
            .lock()
            .unwrap()
            .push(commitment.to_string());
        self.find_by_cosigner_commitment_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(vec![]))
    }

    async fn set_pause(
        &self,
        _account_id: &str,
        now: chrono::DateTime<chrono::Utc>,
        reason: &str,
    ) -> StdResult<crate::services::account_status::PauseTransition, String> {
        Ok(crate::services::account_status::PauseTransition {
            before_state: crate::services::account_status::AccountStatus::Active,
            after_state: crate::services::account_status::AccountStatus::Paused,
            paused_at: Some(now),
            paused_reason: Some(reason.to_string()),
        })
    }

    async fn clear_pause(
        &self,
        _account_id: &str,
    ) -> StdResult<crate::services::account_status::PauseTransition, String> {
        Ok(crate::services::account_status::PauseTransition {
            before_state: crate::services::account_status::AccountStatus::Paused,
            after_state: crate::services::account_status::AccountStatus::Active,
            paused_at: None,
            paused_reason: None,
        })
    }

    async fn set_released(
        &self,
        account_id: &str,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> StdResult<bool, String> {
        self.set_released_calls
            .lock()
            .unwrap()
            .push(account_id.to_string());
        Ok(true)
    }

    async fn clear_released(&self, account_id: &str) -> StdResult<(), String> {
        self.clear_released_calls
            .lock()
            .unwrap()
            .push(account_id.to_string());
        self.clear_released_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(()))
    }
}
