use crate::delta_object::DeltaObject;
use crate::metadata::MetadataStore;
use crate::metadata::auth::{Auth, Credentials};
use crate::network::NetworkClient;
use crate::state_object::StateObject;
use crate::storage::StorageBackend;
use async_trait::async_trait;
use miden_objects::account::Account;
use private_state_manager_shared::FromJson;
use std::sync::{Arc, Mutex as StdMutex};

type StdResult<T, E> = std::result::Result<T, E>;
type ApplyDeltaResult = StdResult<(serde_json::Value, String), String>;
type ShouldUpdateAuthResult = StdResult<Option<Auth>, String>;
type PullDeltasResult = StdResult<Vec<DeltaObject>, String>;
type GetMetadataResult = StdResult<Option<crate::metadata::AccountMetadata>, String>;
type ListResult = StdResult<Vec<String>, String>;

#[derive(Clone, Default)]
pub struct MockNetworkClient {
    pub verify_state_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub verify_state_calls: Arc<StdMutex<Vec<(String, serde_json::Value)>>>,
    pub get_state_commitment_responses: Arc<StdMutex<Vec<StdResult<String, String>>>>,
    pub get_state_commitment_calls: Arc<StdMutex<Vec<(String, serde_json::Value)>>>,
    pub validate_credential_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub verify_delta_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub apply_delta_responses: Arc<StdMutex<Vec<ApplyDeltaResult>>>,
    pub should_update_auth_responses: Arc<StdMutex<Vec<ShouldUpdateAuthResult>>>,
}

impl MockNetworkClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_verify_state(self, response: StdResult<(), String>) -> Self {
        self.verify_state_responses.lock().unwrap().push(response);
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

    pub fn with_verify_delta(self, response: StdResult<(), String>) -> Self {
        self.verify_delta_responses.lock().unwrap().push(response);
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

    pub fn get_verify_state_calls(&self) -> Vec<(String, serde_json::Value)> {
        self.verify_state_calls.lock().unwrap().clone()
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
        let commitment_hex = format!("0x{}", hex::encode(account.commitment().as_bytes()));
        Ok(commitment_hex)
    }

    async fn verify_state(
        &mut self,
        account_id: &str,
        state_json: &serde_json::Value,
    ) -> StdResult<(), String> {
        self.verify_state_calls
            .lock()
            .unwrap()
            .push((account_id.to_string(), state_json.clone()));

        self.verify_state_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(()))
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
    ) -> StdResult<(), String> {
        self.validate_credential_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(()))
    }

    async fn should_update_auth(
        &mut self,
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
        Ok("mock_proposal_id".to_string())
    }
}

#[derive(Clone, Default)]
pub struct MockStorageBackend {
    pub submit_state_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub submit_state_calls: Arc<StdMutex<Vec<StateObject>>>,
    pub submit_delta_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub submit_delta_calls: Arc<StdMutex<Vec<DeltaObject>>>,
    pub pull_state_responses: Arc<StdMutex<Vec<StdResult<StateObject, String>>>>,
    pub pull_delta_responses: Arc<StdMutex<Vec<StdResult<DeltaObject, String>>>>,
    pub pull_deltas_after_responses: Arc<StdMutex<Vec<PullDeltasResult>>>,
    pub submit_delta_proposal_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub submit_delta_proposal_calls: Arc<StdMutex<Vec<(String, DeltaObject)>>>,
    pub pull_delta_proposal_responses: Arc<StdMutex<Vec<StdResult<DeltaObject, String>>>>,
    pub pull_delta_proposal_calls: Arc<StdMutex<Vec<(String, String)>>>,
    #[allow(clippy::type_complexity)]
    pub pull_all_delta_proposals_responses: Arc<StdMutex<Vec<StdResult<Vec<DeltaObject>, String>>>>,
    pub pull_all_delta_proposals_calls: Arc<StdMutex<Vec<String>>>,
    pub update_delta_proposal_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub update_delta_proposal_calls: Arc<StdMutex<Vec<(String, DeltaObject)>>>,
    pub delete_delta_proposal_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub delete_delta_proposal_calls: Arc<StdMutex<Vec<(String, String)>>>,
}

impl MockStorageBackend {
    pub fn new() -> Self {
        Self::default()
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
}

#[async_trait]
impl StorageBackend for MockStorageBackend {
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
            .unwrap_or_else(|| Err("No delta found".to_string()))
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
            .unwrap_or_else(|| Err("Mock: No proposal found".to_string()))
    }

    async fn pull_all_delta_proposals(&self, account_id: &str) -> Result<Vec<DeltaObject>, String> {
        self.pull_all_delta_proposals_calls
            .lock()
            .unwrap()
            .push(account_id.to_string());
        self.pull_all_delta_proposals_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(vec![]))
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

    async fn delete_delta(&self, _account_id: &str, _nonce: u64) -> Result<(), String> {
        Ok(())
    }

    async fn update_delta_status(
        &self,
        _account_id: &str,
        _nonce: u64,
        _status: crate::delta_object::DeltaStatus,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct MockMetadataStore {
    pub get_responses: Arc<StdMutex<Vec<GetMetadataResult>>>,
    pub get_calls: Arc<StdMutex<Vec<String>>>,
    pub set_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub set_calls: Arc<StdMutex<Vec<crate::metadata::AccountMetadata>>>,
    pub list_responses: Arc<StdMutex<Vec<ListResult>>>,
    pub list_with_pending_candidates_responses: Arc<StdMutex<Vec<ListResult>>>,
    pub update_timestamp_cas_responses: Arc<StdMutex<Vec<StdResult<bool, String>>>>,
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

    pub fn get_get_calls(&self) -> Vec<String> {
        self.get_calls.lock().unwrap().clone()
    }

    pub fn get_set_calls(&self) -> Vec<crate::metadata::AccountMetadata> {
        self.set_calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl MetadataStore for MockMetadataStore {
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
        _now: &str,
    ) -> StdResult<bool, String> {
        self.update_timestamp_cas_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(true)) // Default to success
    }
}
