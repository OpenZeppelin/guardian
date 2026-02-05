use crate::delta_object::DeltaObject;
use crate::error::PsmError;
use crate::metadata::auth::{Auth, AuthHeader, Credentials};
use crate::services::{
    self, ConfigureAccountParams, GetDeltaParams, GetDeltaProposalsParams, GetDeltaSinceParams,
    GetStateParams, PushDeltaParams, PushDeltaProposalParams, SignDeltaProposalParams,
};
use crate::state::AppState;
use crate::state_object::StateObject;
use axum::{Json, extract::Query, extract::State, http::StatusCode};
use private_state_manager_shared::{ProposalSignature, SignatureScheme};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct ConfigureRequest {
    pub account_id: String,
    pub auth: Auth,
    pub initial_state: serde_json::Value,
}

impl From<ConfigureRequest> for ConfigureAccountParams {
    fn from(req: ConfigureRequest) -> Self {
        Self {
            account_id: req.account_id,
            auth: req.auth,
            initial_state: req.initial_state,
            // Credential will be set from AuthHeader
            credential: Credentials::signature(String::new(), String::new(), 0),
        }
    }
}

#[derive(Deserialize)]
pub struct DeltaQuery {
    pub account_id: String,
    pub nonce: u64,
}

#[derive(Deserialize)]
pub struct StateQuery {
    pub account_id: String,
}

#[derive(Deserialize)]
pub struct ProposalQuery {
    pub account_id: String,
}

#[derive(Deserialize)]
pub struct DeltaProposalRequest {
    pub account_id: String,
    pub nonce: u64,
    pub delta_payload: serde_json::Value,
}

#[derive(Deserialize)]
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
}

pub async fn configure(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Json(payload): Json<ConfigureRequest>,
) -> Result<Json<ConfigureResponse>, PsmError> {
    let mut params = ConfigureAccountParams::from(payload);
    params.credential = credentials;

    let response = services::configure_account(&state, params).await?;
    Ok(Json(ConfigureResponse {
        success: true,
        message: format!("Account '{}' configured successfully", response.account_id),
        ack_pubkey: Some(response.ack_pubkey),
        ack_commitment: Some(response.ack_commitment),
    }))
}

pub async fn push_delta(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Json(payload): Json<DeltaObject>,
) -> Result<Json<DeltaObject>, PsmError> {
    let params = PushDeltaParams {
        delta: payload,
        credentials,
    };

    let response = services::push_delta(&state, params).await?;
    Ok(Json(response.delta))
}

pub async fn get_delta(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Query(query): Query<DeltaQuery>,
) -> Result<Json<DeltaObject>, PsmError> {
    let params = GetDeltaParams {
        account_id: query.account_id,
        nonce: query.nonce,
        credentials,
    };

    let response = services::get_delta(&state, params).await?;
    Ok(Json(response.delta))
}

pub async fn get_delta_since(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Query(query): Query<DeltaQuery>,
) -> Result<Json<DeltaObject>, PsmError> {
    let params = GetDeltaSinceParams {
        account_id: query.account_id,
        from_nonce: query.nonce,
        credentials,
    };

    let response = services::get_delta_since(&state, params).await?;
    Ok(Json(response.merged_delta))
}

pub async fn get_state(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Query(query): Query<StateQuery>,
) -> Result<Json<StateObject>, PsmError> {
    let params = GetStateParams {
        account_id: query.account_id,
        credentials,
    };

    let response = services::get_state(&state, params).await?;
    Ok(Json(response.state))
}

#[derive(Deserialize)]
pub struct PubkeyQuery {
    pub scheme: Option<String>,
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

#[derive(Debug, Serialize)]
pub struct DeltaProposalResponse {
    pub delta: DeltaObject,
    pub commitment: String,
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
) -> Result<Json<DeltaProposalResponse>, PsmError> {
    let params = PushDeltaProposalParams {
        account_id: payload.account_id,
        nonce: payload.nonce,
        delta_payload: payload.delta_payload,
        credentials,
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
) -> Result<Json<ProposalsResponse>, PsmError> {
    let params = GetDeltaProposalsParams {
        account_id: query.account_id,
        credentials,
    };

    let response = services::get_delta_proposals(&state, params).await?;
    Ok(Json(ProposalsResponse {
        proposals: response.proposals,
    }))
}

pub async fn sign_delta_proposal(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Json(payload): Json<SignProposalRequest>,
) -> Result<Json<DeltaObject>, PsmError> {
    let params = SignDeltaProposalParams {
        account_id: payload.account_id,
        commitment: payload.commitment,
        signature: payload.signature,
        credentials,
    };

    let response = services::sign_delta_proposal(&state, params).await?;
    Ok(Json(response.delta))
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::delta_object::DeltaStatus;
    use crate::metadata::AccountMetadata;
    use crate::state_object::StateObject;
    use crate::testing::fixtures;
    use crate::testing::helpers::{create_test_app_state_with_mocks, generate_falcon_signature};
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
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
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
        }
    }

    #[tokio::test]
    async fn test_get_pubkey_success() {
        let (state, _storage, _network, _metadata) = create_test_state();
        let (status, Json(response)) =
            get_pubkey(State(state), Query(PubkeyQuery { scheme: None })).await;

        assert_eq!(status, StatusCode::OK);
        assert!(!response.commitment.is_empty());
        assert!(response.commitment.starts_with("0x"));
        assert!(response.pubkey.is_none());
    }

    #[tokio::test]
    async fn test_configure_success() {
        let (state, _storage, _network, _metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(&account_id);

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();

        let request = ConfigureRequest {
            account_id: account_id.clone(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment],
            },
            initial_state: account_json,
        };

        let credentials = Credentials::signature(pubkey, signature, timestamp);
        let Json(response) = configure(State(state), AuthHeader(credentials), Json(request))
            .await
            .unwrap();

        assert!(response.success);
        assert!(response.ack_pubkey.is_some());
        assert!(response.message.contains("configured successfully"));
    }

    #[tokio::test]
    async fn test_push_delta_proposal_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(&account_id);

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
            account_id,
            nonce: 1,
            delta_payload: serde_json::json!({
                "tx_summary": delta_fixture["delta_payload"],
                "signatures": []
            }),
        };

        let credentials = Credentials::signature(pubkey, signature, timestamp);
        let Json(response) =
            push_delta_proposal(State(state), AuthHeader(credentials), Json(request))
                .await
                .unwrap();

        assert_eq!(response.delta.nonce, 1);
        assert!(!response.commitment.is_empty());
    }

    #[tokio::test]
    async fn test_push_delta_proposal_missing_tx_summary() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(&account_id);

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
            account_id,
            nonce: 1,
            delta_payload: serde_json::json!({"signatures": []}),
        };

        let credentials = Credentials::signature(pubkey, signature, timestamp);
        let err = push_delta_proposal(State(state), AuthHeader(credentials), Json(request))
            .await
            .unwrap_err();

        assert_eq!(err.http_status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_get_delta_proposals_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(&account_id);

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
            status: DeltaStatus::pending("2024-11-14T12:00:00Z".to_string(), pubkey.clone()),
        };

        let _storage = storage.with_pull_all_delta_proposals(Ok(vec![pending_delta]));

        let query = ProposalQuery {
            account_id: account_id.clone(),
        };

        let credentials = Credentials::signature(pubkey, signature, timestamp);
        let Json(response) =
            get_delta_proposals(State(state), AuthHeader(credentials), Query(query))
                .await
                .unwrap();

        assert_eq!(response.proposals.len(), 1);
        assert_eq!(response.proposals[0].account_id, account_id);
    }

    #[tokio::test]
    async fn test_get_delta_proposals_empty() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let _storage = storage.with_pull_all_delta_proposals(Ok(vec![]));

        let query = ProposalQuery { account_id };

        let credentials = Credentials::signature(pubkey, signature, timestamp);
        let Json(response) =
            get_delta_proposals(State(state), AuthHeader(credentials), Query(query))
                .await
                .unwrap();

        assert_eq!(response.proposals.len(), 0);
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_not_found() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let _storage = storage.with_pull_delta_proposal(Err("Proposal not found".to_string()));

        let dummy_sig = format!("0x{}", "a".repeat(666));
        let request = SignProposalRequest {
            account_id,
            commitment: "nonexistent_proposal".to_string(),
            signature: ProposalSignature::Falcon {
                signature: dummy_sig,
            },
        };

        let credentials = Credentials::signature(pubkey, signature, timestamp);
        let err = sign_delta_proposal(State(state), AuthHeader(credentials), Json(request))
            .await
            .unwrap_err();

        assert_eq!(err.http_status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_push_delta_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(&account_id);

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

        let credentials = Credentials::signature(pubkey, signature, timestamp);
        let Json(response) = push_delta(State(state), AuthHeader(credentials), Json(test_delta))
            .await
            .unwrap();

        assert_eq!(response.account_id, account_id);
    }

    #[tokio::test]
    async fn test_get_delta_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(&account_id);

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

        let credentials = Credentials::signature(pubkey, signature, timestamp);
        let Json(response) = get_delta(State(state), AuthHeader(credentials), Query(query))
            .await
            .unwrap();

        assert_eq!(response.account_id, account_id);
        assert_eq!(response.nonce, 1);
    }

    #[tokio::test]
    async fn test_get_delta_not_found() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let _storage = storage.with_pull_delta(Err("Delta not found".to_string()));

        let query = DeltaQuery {
            account_id,
            nonce: 999,
        };

        let credentials = Credentials::signature(pubkey, signature, timestamp);
        let err = get_delta(State(state), AuthHeader(credentials), Query(query))
            .await
            .unwrap_err();

        assert_eq!(err.http_status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_state_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(&account_id);

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

        let credentials = Credentials::signature(pubkey, signature, timestamp);
        let Json(response) = get_state(State(state), AuthHeader(credentials), Query(query))
            .await
            .unwrap();

        assert_eq!(response.account_id, account_id);
    }

    #[tokio::test]
    async fn test_get_state_not_found() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![commitment],
        ))));

        let _storage = storage.with_pull_state(Err("State not found".to_string()));

        let query = StateQuery { account_id };

        let credentials = Credentials::signature(pubkey, signature, timestamp);
        let err = get_state(State(state), AuthHeader(credentials), Query(query))
            .await
            .unwrap_err();

        assert_eq!(err.http_status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_delta_since_success() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (pubkey, commitment, signature, timestamp) = generate_falcon_signature(&account_id);

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

        let credentials = Credentials::signature(pubkey, signature, timestamp);
        let Json(response) = get_delta_since(State(state), AuthHeader(credentials), Query(query))
            .await
            .unwrap();

        assert_eq!(response.account_id, account_id);
    }
}
