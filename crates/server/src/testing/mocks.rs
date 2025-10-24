use crate::metadata::MetadataStore;
use crate::metadata::auth::{Auth, Credentials};
use crate::network::NetworkClient;
use crate::storage::{AccountState, StorageBackend};
use async_trait::async_trait;
use std::sync::{Arc, Mutex as StdMutex};

type StdResult<T, E> = std::result::Result<T, E>;

#[derive(Clone, Default)]
pub struct MockNetworkClient {
    pub verify_state_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub verify_state_calls: Arc<StdMutex<Vec<(String, serde_json::Value)>>>,
    pub get_state_commitment_responses: Arc<StdMutex<Vec<StdResult<String, String>>>>,
    pub get_state_commitment_calls: Arc<StdMutex<Vec<(String, serde_json::Value)>>>,
    pub validate_credential_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub verify_delta_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub apply_delta_responses: Arc<StdMutex<Vec<StdResult<(serde_json::Value, String), String>>>>,
    pub should_update_auth_responses: Arc<StdMutex<Vec<StdResult<Option<Auth>, String>>>>,
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
        self.get_state_commitment_responses.lock().unwrap().push(response);
        self
    }

    pub fn with_validate_credential(self, response: StdResult<(), String>) -> Self {
        self.validate_credential_responses.lock().unwrap().push(response);
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

        self.get_state_commitment_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok("mock_commitment".to_string()))
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
    ) -> StdResult<Option<Auth>, String> {
        self.should_update_auth_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(None))
    }
}

#[derive(Clone, Default)]
pub struct MockStorageBackend {
    pub submit_state_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub submit_state_calls: Arc<StdMutex<Vec<AccountState>>>,
    pub submit_delta_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub submit_delta_calls: Arc<StdMutex<Vec<crate::storage::DeltaObject>>>,
    pub pull_state_responses: Arc<StdMutex<Vec<StdResult<AccountState, String>>>>,
    pub pull_delta_responses: Arc<StdMutex<Vec<StdResult<crate::storage::DeltaObject, String>>>>,
    pub pull_deltas_after_responses:
        Arc<StdMutex<Vec<StdResult<Vec<crate::storage::DeltaObject>, String>>>>,
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

    pub fn with_pull_state(self, response: StdResult<AccountState, String>) -> Self {
        self.pull_state_responses.lock().unwrap().push(response);
        self
    }

    pub fn with_pull_delta(self, response: StdResult<crate::storage::DeltaObject, String>) -> Self {
        self.pull_delta_responses.lock().unwrap().push(response);
        self
    }

    pub fn with_pull_deltas_after(
        self,
        response: StdResult<Vec<crate::storage::DeltaObject>, String>,
    ) -> Self {
        self.pull_deltas_after_responses
            .lock()
            .unwrap()
            .push(response);
        self
    }

    pub fn get_submit_state_calls(&self) -> Vec<AccountState> {
        self.submit_state_calls.lock().unwrap().clone()
    }

    pub fn get_submit_delta_calls(&self) -> Vec<crate::storage::DeltaObject> {
        self.submit_delta_calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl StorageBackend for MockStorageBackend {
    async fn submit_state(&self, state: &AccountState) -> StdResult<(), String> {
        self.submit_state_calls.lock().unwrap().push(state.clone());
        self.submit_state_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(()))
    }

    async fn submit_delta(&self, delta: &crate::storage::DeltaObject) -> StdResult<(), String> {
        self.submit_delta_calls.lock().unwrap().push(delta.clone());
        self.submit_delta_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Ok(()))
    }

    async fn pull_state(&self, _account_id: &str) -> StdResult<AccountState, String> {
        self.pull_state_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Err("No state found".to_string()))
    }

    async fn pull_delta(
        &self,
        _account_id: &str,
        _nonce: u64,
    ) -> StdResult<crate::storage::DeltaObject, String> {
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
    ) -> StdResult<Vec<crate::storage::DeltaObject>, String> {
        self.pull_deltas_after_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(vec![]))
    }
}

#[derive(Clone, Default)]
pub struct MockMetadataStore {
    pub get_responses:
        Arc<StdMutex<Vec<StdResult<Option<crate::metadata::AccountMetadata>, String>>>>,
    pub get_calls: Arc<StdMutex<Vec<String>>>,
    pub set_responses: Arc<StdMutex<Vec<StdResult<(), String>>>>,
    pub set_calls: Arc<StdMutex<Vec<crate::metadata::AccountMetadata>>>,
    pub list_responses: Arc<StdMutex<Vec<StdResult<Vec<String>, String>>>>,
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
        self.get_responses.lock().unwrap().pop().unwrap_or(Ok(None))
    }

    async fn set(&self, metadata: crate::metadata::AccountMetadata) -> StdResult<(), String> {
        self.set_calls.lock().unwrap().push(metadata);
        self.set_responses.lock().unwrap().pop().unwrap_or(Ok(()))
    }

    async fn list(&self) -> StdResult<Vec<String>, String> {
        self.list_responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(vec![]))
    }
}
