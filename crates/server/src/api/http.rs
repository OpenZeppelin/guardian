use crate::delta_object::DeltaObject;
use crate::error::GuardianError;
use crate::metadata::NetworkConfig;
use crate::metadata::auth::{Auth, AuthHeader, Credentials};
use crate::services::{
    self, ConfigureAccountParams, GetDeltaParams, GetDeltaProposalParams, GetDeltaProposalsParams,
    GetDeltaSinceParams, GetStateParams, LookupAccountParams, PushDeltaParams,
    PushDeltaProposalParams, SignDeltaProposalParams,
};
use crate::state::AppState;
use crate::state_object::StateObject;
use axum::{Json, extract::Query, extract::State, http::StatusCode};
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::{ProposalSignature, SignatureScheme};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct ConfigureRequest {
    pub account_id: String,
    pub auth: Auth,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_config: Option<NetworkConfig>,
    pub initial_state: serde_json::Value,
}

impl From<ConfigureRequest> for ConfigureAccountParams {
    fn from(req: ConfigureRequest) -> Self {
        Self {
            account_id: req.account_id,
            auth: req.auth,
            network_config: req
                .network_config
                .unwrap_or_else(NetworkConfig::miden_default),
            initial_state: req.initial_state,
            // Credential will be set from AuthHeader
            credential: Credentials::signature(String::new(), String::new(), 0),
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct DeltaQuery {
    pub account_id: String,
    pub nonce: u64,
}

#[derive(Deserialize, Serialize)]
pub struct StateQuery {
    pub account_id: String,
}

#[derive(Deserialize, Serialize)]
pub struct LookupQuery {
    pub key_commitment: String,
}

/// Single match in a lookup response. Wraps `account_id` so the response shape
/// can be extended in a forward-compatible way (e.g. adding role tags or
/// per-account metadata) without breaking existing clients.
#[derive(Serialize, Deserialize)]
pub struct LookupAccount {
    pub account_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct LookupResponse {
    pub accounts: Vec<LookupAccount>,
}

#[derive(Deserialize, Serialize)]
pub struct ProposalQuery {
    pub account_id: String,
}

#[derive(Deserialize, Serialize)]
pub struct ProposalItemQuery {
    pub account_id: String,
    pub commitment: String,
}

#[derive(Deserialize, Serialize)]
pub struct DeltaProposalRequest {
    pub account_id: String,
    pub nonce: u64,
    pub delta_payload: serde_json::Value,
}

#[derive(Deserialize, Serialize)]
pub struct SignProposalRequest {
    pub account_id: String,
    pub commitment: String,
    pub signature: ProposalSignature,
}

// Response types
#[derive(Serialize)]
pub struct ConfigureResponse {
    pub success: bool,
    pub message: String,
    pub ack_pubkey: Option<String>,
    pub ack_commitment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub success: bool,
    pub code: &'static str,
    pub error: String,
}

pub async fn configure(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Json(payload): Json<ConfigureRequest>,
) -> (StatusCode, Json<ConfigureResponse>) {
    let request_payload = match request_payload_from_serializable(&payload) {
        Ok(request_payload) => request_payload,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ConfigureResponse {
                    success: false,
                    message: e,
                    ack_pubkey: None,
                    ack_commitment: None,
                    code: None,
                }),
            );
        }
    };

    let mut params = ConfigureAccountParams::from(payload);
    params.credential = request_payload.apply_to(credentials);

    match services::configure_account(&state, params).await {
        Ok(response) => (
            StatusCode::OK,
            Json(ConfigureResponse {
                success: true,
                message: format!("Account '{}' configured successfully", response.account_id),
                ack_pubkey: Some(response.ack_pubkey),
                ack_commitment: Some(response.ack_commitment),
                code: None,
            }),
        ),
        Err(e) => (
            e.http_status(),
            Json(ConfigureResponse {
                success: false,
                message: e.to_string(),
                ack_pubkey: None,
                ack_commitment: None,
                code: Some(e.code()),
            }),
        ),
    }
}

pub async fn push_delta(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<DeltaObject>, GuardianError> {
    let request_payload =
        request_payload_from_value(&payload).map_err(GuardianError::InvalidInput)?;

    let delta: DeltaObject = serde_json::from_value(payload)
        .map_err(|e| GuardianError::InvalidInput(format!("Invalid delta payload: {e}")))?;

    let params = PushDeltaParams {
        delta,
        credentials: request_payload.apply_to(credentials),
    };

    let response = services::push_delta(&state, params).await?;
    Ok(Json(response.delta))
}

pub async fn get_delta(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Query(query): Query<DeltaQuery>,
) -> Result<Json<DeltaObject>, GuardianError> {
    let request_payload =
        request_payload_from_serializable(&query).map_err(GuardianError::InvalidInput)?;

    let params = GetDeltaParams {
        account_id: query.account_id,
        nonce: query.nonce,
        credentials: request_payload.apply_to(credentials),
    };

    let response = services::get_delta(&state, params).await?;
    Ok(Json(response.delta))
}

pub async fn get_delta_since(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Query(query): Query<DeltaQuery>,
) -> Result<Json<DeltaObject>, GuardianError> {
    let request_payload =
        request_payload_from_serializable(&query).map_err(GuardianError::InvalidInput)?;

    let params = GetDeltaSinceParams {
        account_id: query.account_id,
        from_nonce: query.nonce,
        credentials: request_payload.apply_to(credentials),
    };

    let response = services::get_delta_since(&state, params).await?;
    Ok(Json(response.merged_delta))
}

pub async fn get_state(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Query(query): Query<StateQuery>,
) -> Result<Json<StateObject>, GuardianError> {
    let request_payload =
        request_payload_from_serializable(&query).map_err(GuardianError::InvalidInput)?;

    let params = GetStateParams {
        account_id: query.account_id,
        credentials: request_payload.apply_to(credentials),
    };

    let response = services::get_state(&state, params).await?;
    Ok(Json(response.state))
}

/// `GET /state/lookup?key_commitment=<hex>` — resolves a Miden public-key
/// commitment to the set of account IDs whose authorization set contains it.
/// Authentication is by proof-of-possession against the queried commitment.
pub async fn lookup(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Query(query): Query<LookupQuery>,
) -> Result<Json<LookupResponse>, GuardianError> {
    let params = LookupAccountParams {
        key_commitment: query.key_commitment,
        credentials,
    };
    let result = services::lookup_account(&state, params).await?;
    Ok(Json(LookupResponse {
        accounts: result
            .accounts
            .into_iter()
            .map(|account_id| LookupAccount { account_id })
            .collect(),
    }))
}

#[derive(Serialize)]
pub struct PubkeyResponse {
    pub commitment: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pubkey: Option<String>,
}

#[derive(Serialize)]
pub struct ProposalsResponse {
    pub proposals: Vec<DeltaObject>,
}

#[derive(Serialize)]
pub struct DeltaProposalResponse {
    pub delta: DeltaObject,
    pub commitment: String,
}

#[derive(Deserialize, Serialize)]
pub struct PubkeyQuery {
    pub scheme: Option<String>,
}

pub async fn get_pubkey(
    State(state): State<AppState>,
    Query(query): Query<PubkeyQuery>,
) -> (StatusCode, Json<PubkeyResponse>) {
    let scheme = match query.scheme.as_deref() {
        Some(s) if s.eq_ignore_ascii_case("ecdsa") => SignatureScheme::Ecdsa,
        _ => SignatureScheme::Falcon,
    };
    let commitment = state.ack.commitment(&scheme);
    let pubkey = if matches!(scheme, SignatureScheme::Ecdsa) {
        Some(state.ack.pubkey(&scheme))
    } else {
        None
    };
    (StatusCode::OK, Json(PubkeyResponse { commitment, pubkey }))
}

pub async fn push_delta_proposal(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Json(payload): Json<DeltaProposalRequest>,
) -> Result<Json<DeltaProposalResponse>, GuardianError> {
    let request_payload =
        request_payload_from_serializable(&payload).map_err(GuardianError::InvalidInput)?;

    let params = PushDeltaProposalParams {
        account_id: payload.account_id,
        nonce: payload.nonce,
        delta_payload: payload.delta_payload,
        credentials: request_payload.apply_to(credentials),
    };

    let response = services::push_delta_proposal(&state, params).await?;
    Ok(Json(DeltaProposalResponse {
        delta: response.delta,
        commitment: response.commitment,
    }))
}

pub async fn get_delta_proposals(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Query(query): Query<ProposalQuery>,
) -> Result<Json<ProposalsResponse>, GuardianError> {
    let request_payload =
        request_payload_from_serializable(&query).map_err(GuardianError::InvalidInput)?;

    let params = GetDeltaProposalsParams {
        account_id: query.account_id,
        credentials: request_payload.apply_to(credentials),
    };

    let response = services::get_delta_proposals(&state, params).await?;
    Ok(Json(ProposalsResponse {
        proposals: response.proposals,
    }))
}

pub async fn get_delta_proposal(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Query(query): Query<ProposalItemQuery>,
) -> Result<Json<DeltaObject>, GuardianError> {
    let request_payload =
        request_payload_from_serializable(&query).map_err(GuardianError::InvalidInput)?;

    let params = GetDeltaProposalParams {
        account_id: query.account_id,
        commitment: query.commitment,
        credentials: request_payload.apply_to(credentials),
    };

    let response = services::get_delta_proposal(&state, params).await?;
    Ok(Json(response.proposal))
}

pub async fn sign_delta_proposal(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Json(payload): Json<SignProposalRequest>,
) -> Result<Json<DeltaObject>, GuardianError> {
    let request_payload =
        request_payload_from_serializable(&payload).map_err(GuardianError::InvalidInput)?;

    let params = SignDeltaProposalParams {
        account_id: payload.account_id,
        commitment: payload.commitment,
        signature: payload.signature,
        credentials: request_payload.apply_to(credentials),
    };

    let response = services::sign_delta_proposal(&state, params).await?;
    Ok(Json(response.delta))
}

struct RequestPayloadParts {
    payload: AuthRequestPayload,
    bytes: Vec<u8>,
}

impl RequestPayloadParts {
    fn apply_to(self, credentials: Credentials) -> Credentials {
        credentials
            .with_request_payload(self.payload)
            .with_request_payload_bytes(self.bytes)
    }
}

fn request_payload_from_serializable<T: Serialize>(
    value: &T,
) -> Result<RequestPayloadParts, String> {
    let json = serde_json::to_value(value)
        .map_err(|e| format!("Failed to convert payload to JSON value: {e}"))?;
    request_payload_from_value(&json)
}

fn request_payload_from_value(value: &serde_json::Value) -> Result<RequestPayloadParts, String> {
    let canonical = canonicalize_json(value);
    let bytes =
        serde_json::to_vec(&canonical).map_err(|e| format!("Failed to serialize JSON: {e}"))?;
    Ok(RequestPayloadParts {
        payload: AuthRequestPayload::from_bytes(&bytes),
        bytes,
    })
}

fn canonicalize_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys = map.keys().cloned().collect::<Vec<_>>();
            keys.sort();

            let mut sorted = serde_json::Map::with_capacity(map.len());
            for key in keys {
                let item = map
                    .get(&key)
                    .expect("key collected from map must always exist");
                sorted.insert(key, canonicalize_json(item));
            }
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonicalize_json).collect())
        }
        _ => value.clone(),
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::delta_object::DeltaStatus;
    use crate::metadata::AccountMetadata;
    use crate::state_object::StateObject;
    use crate::testing::fixtures;
    use crate::testing::helpers::{TestSigner, create_test_app_state_with_mocks};
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn create_test_state() -> (
        AppState,
        MockStorageBackend,
        MockNetworkClient,
        MockMetadataStore,
    ) {
        let storage = MockStorageBackend::new();
        let network = MockNetworkClient::new();
        let metadata = MockMetadataStore::new();

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(Mutex::new(network.clone())),
            Arc::new(metadata.clone()),
        );

        (state, storage, network, metadata)
    }

    fn create_account_metadata(
        account_id: String,
        cosigner_commitments: Vec<String>,
    ) -> AccountMetadata {
        AccountMetadata {
            account_id,
            auth: Auth::MidenFalconRpo {
                cosigner_commitments,
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
            paused_at: None,
            paused_reason: None,
        }
    }

    fn create_state_object(
        account_id: String,
        commitment: String,
        state_json: serde_json::Value,
    ) -> StateObject {
        StateObject {
            account_id,
            commitment,
            state_json,
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            auth_scheme: String::new(),
        }
    }

    fn create_test_delta(account_id: &str, nonce: u64) -> DeltaObject {
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        DeltaObject {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392"
                .to_string(),
            new_commitment: Some(
                "0x8fa68eabc9817e17900a7f1f705c1ecdeef6ab64c15ca1b66447272fb8fa49b2".to_string(),
            ),
            delta_payload: delta_fixture["delta_payload"].clone(),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::canonical("2024-11-14T12:00:00Z".to_string()),
            metadata: None,
        }
    }

    fn signed_credentials<T: serde::Serialize>(
        signer: &TestSigner,
        account_id: &str,
        request: &T,
    ) -> Credentials {
        let (signature, timestamp) = signer.sign_json_payload(account_id, request);
        Credentials::signature(signer.pubkey_hex.clone(), signature, timestamp)
    }

    #[tokio::test]
    async fn test_get_pubkey_success() {
        let (state, _storage, _network, _metadata) = create_test_state();
        let (status, Json(response)) =
            get_pubkey(State(state), Query(PubkeyQuery { scheme: None })).await;

        assert_eq!(status, StatusCode::OK);
        assert!(!response.commitment.is_empty());
        assert!(response.commitment.starts_with("0x"));
    }

    #[tokio::test]
    async fn test_get_pubkey_success_for_ecdsa() {
        let (state, _storage, _network, _metadata) = create_test_state();
        let (status, Json(response)) = get_pubkey(
            State(state),
            Query(PubkeyQuery {
                scheme: Some("ecdsa".to_string()),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(response.commitment.starts_with("0x"));
        assert_eq!(response.commitment.len(), 66);
        assert!(response.pubkey.is_some());
        assert!(response.pubkey.unwrap().starts_with("0x"));
    }

    #[tokio::test]
    async fn test_configure_success() {
        let (state, _storage, _network, _metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();

        let request = ConfigureRequest {
            account_id: account_id.clone(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment],
            },
            network_config: None,
            initial_state: account_json,
        };

        let credentials = signed_credentials(&signer, &account_id, &request);
        let (status, Json(response)) =
            configure(State(state), AuthHeader(credentials), Json(request)).await;

        assert_eq!(status, StatusCode::OK);
        assert!(response.success);
        assert!(response.ack_pubkey.is_some());
        assert!(response.message.contains("configured successfully"));
    }

    #[tokio::test]
    async fn test_push_delta_proposal_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let _storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392".to_string(),
            account_json,
        )));

        let request = DeltaProposalRequest {
            account_id: account_id.clone(),
            nonce: 1,
            delta_payload: serde_json::json!({
                "tx_summary": delta_fixture["delta_payload"],
                "signatures": [],
                "metadata": {
                    "proposal_type": "change_threshold",
                    "target_threshold": 1,
                    "signer_commitments": [signer.commitment_hex.clone()]
                }
            }),
        };

        let credentials = signed_credentials(&signer, &account_id, &request);
        let Json(response) =
            push_delta_proposal(State(state), AuthHeader(credentials), Json(request))
                .await
                .expect("push_delta_proposal should succeed");

        assert_eq!(response.delta.nonce, 1);
        assert!(!response.commitment.is_empty());
    }

    #[tokio::test]
    async fn test_push_delta_proposal_missing_tx_summary() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let _storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            "0x123".to_string(),
            account_json,
        )));

        let request = DeltaProposalRequest {
            account_id: account_id.clone(),
            nonce: 1,
            delta_payload: serde_json::json!({
                "signatures": [],
                "metadata": {
                    "proposal_type": "change_threshold",
                    "target_threshold": 1,
                    "signer_commitments": [signer.commitment_hex.clone()]
                }
            }),
        };

        let credentials = signed_credentials(&signer, &account_id, &request);
        let result =
            push_delta_proposal(State(state), AuthHeader(credentials), Json(request)).await;
        let err = match result {
            Ok(_) => panic!("missing tx_summary should reject"),
            Err(err) => err,
        };

        assert_eq!(err.http_status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_get_delta_proposals_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        // Create a pending delta proposal
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let pending_delta = DeltaObject {
            account_id: account_id.clone(),
            nonce: 1,
            prev_commitment: "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392"
                .to_string(),
            new_commitment: None,
            delta_payload: delta_fixture["delta_payload"].clone(),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::pending(
                "2024-11-14T12:00:00Z".to_string(),
                signer.pubkey_hex.clone(),
            ),
            metadata: None,
        };

        let _storage = storage.with_pull_all_delta_proposals(Ok(vec![pending_delta]));

        let query = ProposalQuery {
            account_id: account_id.clone(),
        };

        let credentials = signed_credentials(&signer, &account_id, &query);
        let Json(response) =
            get_delta_proposals(State(state), AuthHeader(credentials), Query(query))
                .await
                .expect("get_delta_proposals should succeed");

        assert_eq!(response.proposals.len(), 1);
        assert_eq!(response.proposals[0].account_id, account_id);
    }

    #[tokio::test]
    async fn test_get_delta_proposals_empty() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let _storage = storage.with_pull_all_delta_proposals(Ok(vec![]));

        let query = ProposalQuery {
            account_id: account_id.clone(),
        };

        let credentials = signed_credentials(&signer, &account_id, &query);
        let Json(response) =
            get_delta_proposals(State(state), AuthHeader(credentials), Query(query))
                .await
                .expect("get_delta_proposals should succeed with empty result");

        assert_eq!(response.proposals.len(), 0);
    }

    #[tokio::test]
    async fn test_get_delta_proposal_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment.clone()],
        ))));

        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let pending_delta = DeltaObject {
            account_id: account_id.clone(),
            nonce: 1,
            prev_commitment: "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392"
                .to_string(),
            new_commitment: None,
            delta_payload: delta_fixture["delta_payload"].clone(),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::pending(
                "2024-11-14T12:00:00Z".to_string(),
                signer.pubkey_hex.clone(),
            ),
            metadata: None,
        };

        let _storage = storage.with_pull_delta_proposal(Ok(pending_delta));

        let query = ProposalItemQuery {
            account_id: account_id.clone(),
            commitment: "0xproposal".to_string(),
        };

        let credentials = signed_credentials(&signer, &account_id, &query);
        let Json(response) =
            get_delta_proposal(State(state), AuthHeader(credentials), Query(query))
                .await
                .expect("get_delta_proposal should succeed");

        assert_eq!(response.account_id, account_id);
        assert_eq!(response.nonce, 1);
    }

    #[tokio::test]
    async fn test_get_delta_proposal_not_found() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let _storage = storage.with_pull_delta_proposal(Err("Proposal not found".to_string()));

        let query = ProposalItemQuery {
            account_id: account_id.clone(),
            commitment: "0xmissing".to_string(),
        };

        let credentials = signed_credentials(&signer, &account_id, &query);
        let err =
            match get_delta_proposal(State(state), AuthHeader(credentials), Query(query)).await {
                Ok(_) => panic!("get_delta_proposal should fail when proposal is missing"),
                Err(err) => err,
            };

        assert_eq!(err.http_status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_delta_proposal_unauthorized() {
        let (state, _storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![signer.commitment_hex.clone()],
        ))));

        let query = ProposalItemQuery {
            account_id: account_id.clone(),
            commitment: "0xproposal".to_string(),
        };

        let credentials = signed_credentials(&signer, &account_id, &query);
        let (pubkey, _signature, timestamp) = credentials.as_signature().unwrap();
        let invalid_credentials =
            Credentials::signature(pubkey.to_string(), "0xdeadbeef".to_string(), timestamp);
        let err =
            match get_delta_proposal(State(state), AuthHeader(invalid_credentials), Query(query))
                .await
            {
                Ok(_) => panic!("get_delta_proposal should fail with invalid credentials"),
                Err(err) => err,
            };

        assert_eq!(err.http_status(), StatusCode::UNAUTHORIZED);
        assert!(matches!(err, GuardianError::AuthenticationFailed(_)));
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_not_found() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let _storage = storage.with_pull_delta_proposal(Err("Proposal not found".to_string()));

        let dummy_sig = format!("0x{}", "a".repeat(666));
        let request = SignProposalRequest {
            account_id: account_id.clone(),
            commitment: "nonexistent_proposal".to_string(),
            signature: ProposalSignature::Falcon {
                signature: dummy_sig,
            },
        };

        let credentials = signed_credentials(&signer, &account_id, &request);
        let result =
            sign_delta_proposal(State(state), AuthHeader(credentials), Json(request)).await;
        assert!(result.is_err(), "sign on missing proposal should fail");
    }

    #[tokio::test]
    async fn test_push_delta_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let test_delta = create_test_delta(&account_id, 1);

        let storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            test_delta.prev_commitment.clone(),
            account_json,
        )));
        let _storage = storage.with_pull_deltas_after(Ok(vec![]));

        let test_delta_value = serde_json::to_value(&test_delta).unwrap();
        let credentials = signed_credentials(&signer, &account_id, &test_delta_value);
        let Json(response) = push_delta(
            State(state),
            AuthHeader(credentials),
            Json(test_delta_value),
        )
        .await
        .expect("push_delta should succeed");

        assert_eq!(response.account_id, account_id);
    }

    #[tokio::test]
    async fn test_get_delta_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let test_delta = create_test_delta(&account_id, 1);
        let _storage = storage.with_pull_delta(Ok(test_delta));

        let query = DeltaQuery {
            account_id: account_id.clone(),
            nonce: 1,
        };

        let credentials = signed_credentials(&signer, &account_id, &query);
        let Json(response) = get_delta(State(state), AuthHeader(credentials), Query(query))
            .await
            .expect("get_delta should succeed");

        assert_eq!(response.account_id, account_id);
        assert_eq!(response.nonce, 1);
    }

    #[tokio::test]
    async fn test_get_delta_not_found() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let _storage = storage.with_pull_delta(Err("Delta not found".to_string()));

        let query = DeltaQuery {
            account_id: account_id.clone(),
            nonce: 999,
        };

        let credentials = signed_credentials(&signer, &account_id, &query);
        let err = match get_delta(State(state), AuthHeader(credentials), Query(query)).await {
            Ok(_) => panic!("get_delta should fail when delta is missing"),
            Err(err) => err,
        };

        assert_eq!(err.http_status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_state_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let _storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            "0x123".to_string(),
            account_json,
        )));

        let query = StateQuery {
            account_id: account_id.clone(),
        };

        let credentials = signed_credentials(&signer, &account_id, &query);
        let Json(response) = get_state(State(state), AuthHeader(credentials), Query(query))
            .await
            .expect("get_state should succeed");

        assert_eq!(response.account_id, account_id);
    }

    #[tokio::test]
    async fn test_get_state_not_found() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let _storage = storage.with_pull_state(Err("State not found".to_string()));

        let query = StateQuery {
            account_id: account_id.clone(),
        };

        let credentials = signed_credentials(&signer, &account_id, &query);
        let err = match get_state(State(state), AuthHeader(credentials), Query(query)).await {
            Ok(_) => panic!("get_state should fail when state is missing"),
            Err(err) => err,
        };

        assert_eq!(err.http_status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_delta_since_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let signer = TestSigner::new();
        let commitment = signer.commitment_hex.clone();

        let _account_json: serde_json::Value =
            serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let test_delta = create_test_delta(&account_id, 1);
        let _storage = storage.with_pull_deltas_after(Ok(vec![test_delta]));

        let query = DeltaQuery {
            account_id: account_id.clone(),
            nonce: 0,
        };

        let credentials = signed_credentials(&signer, &account_id, &query);
        let Json(response) = get_delta_since(State(state), AuthHeader(credentials), Query(query))
            .await
            .expect("get_delta_since should succeed");

        assert_eq!(response.account_id, account_id);
    }
}
