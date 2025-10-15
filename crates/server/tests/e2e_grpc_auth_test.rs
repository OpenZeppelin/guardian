use tonic::{Request, metadata::MetadataValue};
use std::sync::Arc;
use tokio::sync::Mutex;

use server::state::AppState;
use server::grpc::{StateManagerService, state_manager::*};
use server::storage::filesystem::{FilesystemConfig, FilesystemService};
use server::metadata::file_store::FileMetadataStore;

use miden_objects::account::{AccountId, AccountIdVersion, AccountType, AccountStorageMode};
use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
use miden_objects::crypto::hash::rpo::Rpo256;
use miden_objects::{Felt, FieldElement, Word};
use miden_objects::utils::Serializable;

/// Helper to create a test account ID
fn create_test_account_id() -> (AccountId, String) {
    let account_id = AccountId::dummy(
        [0u8; 15],
        AccountIdVersion::Version0,
        AccountType::RegularAccountImmutableCode,
        AccountStorageMode::Private,
    );
    let account_id_hex = account_id.to_hex();
    (account_id, account_id_hex)
}

/// Helper to generate a Falcon key pair and signature
fn generate_falcon_signature(account_id_hex: &str) -> (String, String, String) {
    // Generate key pair
    let secret_key = SecretKey::new();
    let public_key = secret_key.public_key();

    // Create message digest (same as in verification)
    let account_id = AccountId::from_hex(account_id_hex).expect("Valid account ID");
    let account_id_felts: [Felt; 2] = account_id.into();

    let message_elements = vec![
        account_id_felts[0],
        account_id_felts[1],
        Felt::ZERO,
        Felt::ZERO,
    ];

    let digest = Rpo256::hash_elements(&message_elements);
    let message: Word = digest.into();

    // Sign the message
    let signature = secret_key.sign(message);

    // Convert to hex strings
    let pubkey_word: Word = public_key.into();
    let pubkey_hex = format!("0x{}", hex::encode(pubkey_word.to_bytes()));
    let signature_hex = format!("0x{}", hex::encode(&signature.to_bytes()));

    (account_id_hex.to_string(), pubkey_hex, signature_hex)
}

/// Helper to create test app state
async fn create_test_app_state() -> AppState {
    // Create temporary directory for test storage
    let test_dir = std::env::temp_dir().join(format!("psm_test_grpc_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&test_dir).expect("Failed to create test directory");

    let config = FilesystemConfig { app_path: test_dir.clone() };
    let storage = FilesystemService::new(config).await.expect("Failed to create storage");
    let metadata = FileMetadataStore::new(test_dir).await.expect("Failed to create metadata");

    AppState {
        storage: Arc::new(storage),
        metadata: Arc::new(Mutex::new(metadata)),
    }
}

/// Helper to create gRPC service
fn create_grpc_service(state: AppState) -> StateManagerService {
    StateManagerService { app_state: state }
}

/// Helper to create a request with auth metadata
fn create_request_with_auth<T>(payload: T, pubkey: &str, sig: &str) -> Request<T> {
    let mut request = Request::new(payload);
    let metadata = request.metadata_mut();

    metadata.insert(
        "x-pubkey",
        MetadataValue::try_from(pubkey).expect("Valid pubkey metadata"),
    );
    metadata.insert(
        "x-signature",
        MetadataValue::try_from(sig).expect("Valid sig metadata"),
    );

    request
}

#[tokio::test]
async fn test_grpc_configure_account() {
    use server::grpc::state_manager::state_manager_server::StateManager;

    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex) = create_test_account_id();

    let configure_req = ConfigureRequest {
        account_id: account_id_hex,
        auth_type: "MidenFalconRpo".to_string(),
        initial_state: r#"{"balance": 0}"#.to_string(),
        storage_type: "filesystem".to_string(),
        cosigner_pubkeys: vec![],
    };

    let request = Request::new(configure_req);
    let response = service.configure(request).await;

    assert!(response.is_ok(), "Configure should succeed");
    let response = response.unwrap().into_inner();
    assert!(response.success, "Configure response should be successful");
}

#[tokio::test]
async fn test_grpc_configure_and_push_delta_with_auth() {
    use server::grpc::state_manager::state_manager_server::StateManager;

    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex) = create_test_account_id();
    let (_, pubkey_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Step 1: Configure account with the cosigner public key
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth_type: "MidenFalconRpo".to_string(),
        initial_state: r#"{"balance": 0}"#.to_string(),
        storage_type: "filesystem".to_string(),
        cosigner_pubkeys: vec![pubkey_hex.clone()],
    };

    let configure_response = service.configure(Request::new(configure_req)).await;
    assert!(configure_response.is_ok(), "Configure should succeed");
    assert!(configure_response.unwrap().into_inner().success);

    // Step 2: Push a delta with authentication metadata
    let push_req = PushDeltaRequest {
        account_id: account_id_hex,
        nonce: 1,
        prev_commitment: "0x0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        delta_hash: "0x1111111111111111111111111111111111111111111111111111111111111111".to_string(),
        delta_payload: r#"{"changes": ["balance_update"]}"#.to_string(),
        ack_sig: "".to_string(),
        candidate_at: "2024-01-01T00:00:00Z".to_string(),
        canonical_at: None,
        discarded_at: None,
    };

    let request = create_request_with_auth(push_req, &pubkey_hex, &signature_hex);
    let push_response = service.push_delta(request).await;

    assert!(push_response.is_ok(), "Push delta should succeed with valid auth");
    let push_response = push_response.unwrap().into_inner();
    assert!(push_response.success, "Push response should be successful: {}", push_response.message);
}

#[tokio::test]
async fn test_grpc_push_delta_unauthorized_cosigner() {
    use server::grpc::state_manager::state_manager_server::StateManager;

    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex) = create_test_account_id();

    // Generate two different key pairs
    let (_, authorized_pubkey, _) = generate_falcon_signature(&account_id_hex);
    let (_, unauthorized_pubkey, unauthorized_sig) = generate_falcon_signature(&account_id_hex);

    // Configure account with ONLY the authorized pubkey
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth_type: "MidenFalconRpo".to_string(),
        initial_state: r#"{"balance": 0}"#.to_string(),
        storage_type: "filesystem".to_string(),
        cosigner_pubkeys: vec![authorized_pubkey], // Only this key is authorized
    };

    let configure_response = service.configure(Request::new(configure_req)).await;
    assert!(configure_response.is_ok());
    assert!(configure_response.unwrap().into_inner().success);

    // Try to push delta with UNAUTHORIZED key
    let push_req = PushDeltaRequest {
        account_id: account_id_hex,
        nonce: 1,
        prev_commitment: "0x0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        delta_hash: "0x1111111111111111111111111111111111111111111111111111111111111111".to_string(),
        delta_payload: r#"{"changes": ["balance_update"]}"#.to_string(),
        ack_sig: "".to_string(),
        candidate_at: "2024-01-01T00:00:00Z".to_string(),
        canonical_at: None,
        discarded_at: None,
    };

    let request = create_request_with_auth(push_req, &unauthorized_pubkey, &unauthorized_sig);
    let push_response = service.push_delta(request).await;

    // Should succeed as a gRPC call but return failure in response
    assert!(push_response.is_ok(), "gRPC call should succeed");
    let push_response = push_response.unwrap().into_inner();
    assert!(!push_response.success, "Push should fail with unauthorized cosigner");
    assert!(push_response.message.contains("not authorized"), "Error message should mention authorization");
}

#[tokio::test]
async fn test_grpc_push_delta_missing_auth_metadata() {
    use server::grpc::state_manager::state_manager_server::StateManager;

    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex) = create_test_account_id();
    let (_, pubkey_hex, _) = generate_falcon_signature(&account_id_hex);

    // Configure account
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth_type: "MidenFalconRpo".to_string(),
        initial_state: r#"{"balance": 0}"#.to_string(),
        storage_type: "filesystem".to_string(),
        cosigner_pubkeys: vec![pubkey_hex],
    };

    let configure_response = service.configure(Request::new(configure_req)).await;
    assert!(configure_response.is_ok());
    assert!(configure_response.unwrap().into_inner().success);

    // Try to push delta WITHOUT auth metadata
    let push_req = PushDeltaRequest {
        account_id: account_id_hex,
        nonce: 1,
        prev_commitment: "0x0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        delta_hash: "0x1111111111111111111111111111111111111111111111111111111111111111".to_string(),
        delta_payload: r#"{"changes": ["balance_update"]}"#.to_string(),
        ack_sig: "".to_string(),
        candidate_at: "2024-01-01T00:00:00Z".to_string(),
        canonical_at: None,
        discarded_at: None,
    };

    // Request WITHOUT auth metadata
    let request = Request::new(push_req);
    let push_response = service.push_delta(request).await;

    // Should fail at the gRPC level (Status error)
    assert!(push_response.is_err(), "Should fail without auth metadata");
    let error = push_response.unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument, "Should be InvalidArgument error");
    assert!(error.message().contains("x-pubkey") || error.message().contains("x-signature"), "Error should mention missing metadata");
}

#[tokio::test]
async fn test_grpc_get_delta_with_auth() {
    use server::grpc::state_manager::state_manager_server::StateManager;

    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex) = create_test_account_id();
    let (_, pubkey_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Configure account
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth_type: "MidenFalconRpo".to_string(),
        initial_state: r#"{"balance": 0}"#.to_string(),
        storage_type: "filesystem".to_string(),
        cosigner_pubkeys: vec![pubkey_hex.clone()],
    };

    service.configure(Request::new(configure_req)).await.unwrap();

    // Push a delta
    let push_req = PushDeltaRequest {
        account_id: account_id_hex.clone(),
        nonce: 1,
        prev_commitment: "0x0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        delta_hash: "0x1111111111111111111111111111111111111111111111111111111111111111".to_string(),
        delta_payload: r#"{"changes": ["balance_update"]}"#.to_string(),
        ack_sig: "".to_string(),
        candidate_at: "2024-01-01T00:00:00Z".to_string(),
        canonical_at: None,
        discarded_at: None,
    };

    service.push_delta(create_request_with_auth(push_req, &pubkey_hex, &signature_hex)).await.unwrap();

    // Get delta with auth
    let get_req = GetDeltaRequest {
        account_id: account_id_hex,
        nonce: 1,
    };

    let request = create_request_with_auth(get_req, &pubkey_hex, &signature_hex);
    let get_response = service.get_delta(request).await;

    assert!(get_response.is_ok(), "Get delta should succeed with valid auth");
    let get_response = get_response.unwrap().into_inner();
    assert!(get_response.success, "Get response should be successful");
    assert!(get_response.delta.is_some(), "Should return delta");
    assert_eq!(get_response.delta.unwrap().nonce, 1, "Should return correct delta");
}
