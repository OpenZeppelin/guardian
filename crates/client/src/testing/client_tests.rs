use crate::auth::FalconRpoSigner;
use crate::proto::auth_config::AuthType;
use crate::testing::mocks::{
    MockStateManagerService, create_mock_account_state, create_mock_delta, start_mock_server,
};
use crate::{
    Auth, AuthConfig, ClientError, ConfigureResponse, GetDeltaProposalsResponse, GetDeltaResponse,
    GetDeltaSinceResponse, GetStateResponse, PsmClient, PushDeltaProposalResponse,
    PushDeltaResponse, SignDeltaProposalResponse,
};
use miden_objects::account::AccountId;
use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
use private_state_manager_shared::ProposalSignature as JsonProposalSignature;
use tonic::Status;

fn create_test_account_id() -> AccountId {
    AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594170").unwrap()
}

fn create_test_auth() -> Auth {
    let secret_key = SecretKey::new();
    Auth::FalconRpoSigner(FalconRpoSigner::new(secret_key))
}

#[tokio::test]
async fn test_get_pubkey_success() {
    let service =
        MockStateManagerService::default().with_get_pubkey(Ok("test_pubkey_123".to_string()));

    let endpoint = start_mock_server(service).await.unwrap();
    let mut client = PsmClient::connect(endpoint).await.unwrap();

    let result = client.get_pubkey().await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "test_pubkey_123");
}

#[tokio::test]
async fn test_get_pubkey_error() {
    let service =
        MockStateManagerService::default().with_get_pubkey(Err(Status::internal("Server error")));

    let endpoint = start_mock_server(service).await.unwrap();
    let mut client = PsmClient::connect(endpoint).await.unwrap();

    let result = client.get_pubkey().await;

    assert!(result.is_err());
    match result.unwrap_err() {
        ClientError::Status(_) => {}
        e => panic!("Expected Status error, got: {:?}", e),
    }
}

#[tokio::test]
async fn test_configure_success() {
    let service = MockStateManagerService::default().with_configure(Ok(ConfigureResponse {
        success: true,
        message: "Account configured".to_string(),
        ack_pubkey: "test_pubkey_123".to_string(),
        ack_commitment: String::new(),
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let auth = create_test_auth();
    let mut client = PsmClient::connect(endpoint).await.unwrap().with_auth(auth);

    let account_id = create_test_account_id();
    let initial_state = serde_json::json!({"balance": 1000});
    let auth_config = AuthConfig {
        auth_type: Some(AuthType::MidenFalconRpo(crate::proto::MidenFalconRpoAuth {
            cosigner_commitments: vec!["0xabc".to_string()],
        })),
    };

    let result = client
        .configure(&account_id, auth_config, initial_state)
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert_eq!(response.message, "Account configured");
    assert_eq!(response.ack_pubkey, "test_pubkey_123");
}

#[tokio::test]
async fn test_configure_server_error() {
    let service = MockStateManagerService::default().with_configure(Ok(ConfigureResponse {
        success: false,
        message: "Account already exists".to_string(),
        ack_pubkey: String::new(),
        ack_commitment: String::new(),
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let auth = create_test_auth();
    let mut client = PsmClient::connect(endpoint).await.unwrap().with_auth(auth);

    let account_id = create_test_account_id();
    let initial_state = serde_json::json!({"balance": 1000});
    let auth_config = AuthConfig {
        auth_type: Some(AuthType::MidenFalconRpo(crate::proto::MidenFalconRpoAuth {
            cosigner_commitments: vec!["0xabc".to_string()],
        })),
    };

    let result = client
        .configure(&account_id, auth_config, initial_state)
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        ClientError::ServerError(msg) => {
            assert_eq!(msg, "Account already exists");
        }
        e => panic!("Expected ServerError, got: {:?}", e),
    }
}

#[tokio::test]
async fn test_push_delta_proposal_success() {
    let mock_delta = create_mock_delta();
    let service = MockStateManagerService::default().with_push_delta_proposal(Ok(
        PushDeltaProposalResponse {
            success: true,
            message: String::new(),
            commitment: "proposal_commitment_123".to_string(),
            delta: Some(mock_delta.clone()),
        },
    ));

    let endpoint = start_mock_server(service).await.unwrap();
    let auth = create_test_auth();
    let mut client = PsmClient::connect(endpoint).await.unwrap().with_auth(auth);

    let account_id = create_test_account_id();
    let delta_payload = serde_json::json!({"tx_summary": {}, "signatures": []});

    let result = client
        .push_delta_proposal(&account_id, 1, delta_payload)
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert_eq!(response.commitment, "proposal_commitment_123");
    assert!(response.delta.is_some());
}

#[tokio::test]
async fn test_get_delta_proposals_success() {
    let mock_delta1 = create_mock_delta();
    let mut mock_delta2 = create_mock_delta();
    mock_delta2.nonce = 2;

    let service = MockStateManagerService::default().with_get_delta_proposals(Ok(
        GetDeltaProposalsResponse {
            success: true,
            message: String::new(),
            proposals: vec![mock_delta1, mock_delta2],
        },
    ));

    let endpoint = start_mock_server(service).await.unwrap();
    let auth = create_test_auth();
    let mut client = PsmClient::connect(endpoint).await.unwrap().with_auth(auth);

    let account_id = create_test_account_id();

    let result = client.get_delta_proposals(&account_id).await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert_eq!(response.proposals.len(), 2);
}

#[tokio::test]
async fn test_get_delta_proposals_empty() {
    let service = MockStateManagerService::default().with_get_delta_proposals(Ok(
        GetDeltaProposalsResponse {
            success: true,
            message: String::new(),
            proposals: vec![],
        },
    ));

    let endpoint = start_mock_server(service).await.unwrap();
    let auth = create_test_auth();
    let mut client = PsmClient::connect(endpoint).await.unwrap().with_auth(auth);

    let account_id = create_test_account_id();

    let result = client.get_delta_proposals(&account_id).await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert_eq!(response.proposals.len(), 0);
}

#[tokio::test]
async fn test_sign_delta_proposal_success() {
    let mock_delta = create_mock_delta();
    let service = MockStateManagerService::default().with_sign_delta_proposal(Ok(
        SignDeltaProposalResponse {
            success: true,
            message: "Signature added".to_string(),
            delta: Some(mock_delta),
        },
    ));

    let endpoint = start_mock_server(service).await.unwrap();
    let auth = create_test_auth();
    let mut client = PsmClient::connect(endpoint).await.unwrap().with_auth(auth);

    let account_id = create_test_account_id();
    let signature = JsonProposalSignature::Falcon {
        signature: "0xabcd".to_string(),
    };

    let result = client
        .sign_delta_proposal(&account_id, "commitment_123", signature)
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert_eq!(response.message, "Signature added");
    assert!(response.delta.is_some());
}

#[tokio::test]
async fn test_push_delta_success() {
    let mock_delta = create_mock_delta();
    let service = MockStateManagerService::default().with_push_delta(Ok(PushDeltaResponse {
        success: true,
        message: "Delta pushed".to_string(),
        delta: Some(mock_delta),
        ack_sig: Some("0xsig".to_string()),
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let auth = create_test_auth();
    let mut client = PsmClient::connect(endpoint).await.unwrap().with_auth(auth);

    let account_id = create_test_account_id();
    let delta_payload = serde_json::json!({"updates": []});

    let result = client
        .push_delta(&account_id, 1, "0x123", delta_payload)
        .await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert_eq!(response.message, "Delta pushed");
    assert!(response.delta.is_some());
}

#[tokio::test]
async fn test_get_delta_success() {
    let mock_delta = create_mock_delta();
    let service = MockStateManagerService::default().with_get_delta(Ok(GetDeltaResponse {
        success: true,
        message: String::new(),
        delta: Some(mock_delta),
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let auth = create_test_auth();
    let mut client = PsmClient::connect(endpoint).await.unwrap().with_auth(auth);

    let account_id = create_test_account_id();

    let result = client.get_delta(&account_id, 1).await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert!(response.delta.is_some());
    assert_eq!(response.delta.unwrap().nonce, 1);
}

#[tokio::test]
async fn test_get_delta_not_found() {
    let service = MockStateManagerService::default().with_get_delta(Ok(GetDeltaResponse {
        success: false,
        message: "Delta not found".to_string(),
        delta: None,
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let auth = create_test_auth();
    let mut client = PsmClient::connect(endpoint).await.unwrap().with_auth(auth);

    let account_id = create_test_account_id();

    let result = client.get_delta(&account_id, 999).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        ClientError::ServerError(msg) => {
            assert_eq!(msg, "Delta not found");
        }
        e => panic!("Expected ServerError, got: {:?}", e),
    }
}

#[tokio::test]
async fn test_get_delta_since_success() {
    let mock_delta = create_mock_delta();
    let service =
        MockStateManagerService::default().with_get_delta_since(Ok(GetDeltaSinceResponse {
            success: true,
            message: String::new(),
            merged_delta: Some(mock_delta),
        }));

    let endpoint = start_mock_server(service).await.unwrap();
    let auth = create_test_auth();
    let mut client = PsmClient::connect(endpoint).await.unwrap().with_auth(auth);

    let account_id = create_test_account_id();

    let result = client.get_delta_since(&account_id, 1).await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert!(response.merged_delta.is_some());
}

#[tokio::test]
async fn test_get_state_success() {
    let mock_state = create_mock_account_state();
    let service = MockStateManagerService::default().with_get_state(Ok(GetStateResponse {
        success: true,
        message: String::new(),
        state: Some(mock_state),
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let auth = create_test_auth();
    let mut client = PsmClient::connect(endpoint).await.unwrap().with_auth(auth);

    let account_id = create_test_account_id();

    let result = client.get_state(&account_id).await;

    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.success);
    assert!(response.state.is_some());
    assert!(response.state.unwrap().state_json.contains("balance"));
}

#[tokio::test]
async fn test_get_state_not_found() {
    let service = MockStateManagerService::default().with_get_state(Ok(GetStateResponse {
        success: false,
        message: "State not found".to_string(),
        state: None,
    }));

    let endpoint = start_mock_server(service).await.unwrap();
    let auth = create_test_auth();
    let mut client = PsmClient::connect(endpoint).await.unwrap().with_auth(auth);

    let account_id = create_test_account_id();

    let result = client.get_state(&account_id).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        ClientError::ServerError(msg) => {
            assert_eq!(msg, "State not found");
        }
        e => panic!("Expected ServerError, got: {:?}", e),
    }
}

#[tokio::test]
async fn test_auth_pubkey_hex_without_auth() {
    let service = MockStateManagerService::default();
    let endpoint = start_mock_server(service).await.unwrap();
    let client = PsmClient::connect(endpoint).await.unwrap();

    let result = client.auth_pubkey_hex();

    assert!(result.is_err());
    match result.unwrap_err() {
        ClientError::InvalidResponse(msg) => {
            assert!(msg.contains("no auth configured"));
        }
        e => panic!("Expected InvalidResponse, got: {:?}", e),
    }
}

#[tokio::test]
async fn test_auth_pubkey_hex_with_auth() {
    let service = MockStateManagerService::default();
    let endpoint = start_mock_server(service).await.unwrap();
    let auth = create_test_auth();
    let expected_pubkey = auth.public_key_hex();
    let client = PsmClient::connect(endpoint).await.unwrap().with_auth(auth);

    let result = client.auth_pubkey_hex();

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), expected_pubkey);
}
