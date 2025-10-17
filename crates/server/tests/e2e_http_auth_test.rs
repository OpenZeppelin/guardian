use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::json;
use std::sync::Arc;
use tower::{Service, ServiceExt}; // For making service calls

use server::api::http;
use server::network::NetworkType;
use server::state::AppState;
use server::storage::filesystem::{FilesystemMetadataStore, FilesystemService};
use server::storage::{StorageBackend, StorageRegistry, StorageType};
use std::collections::HashMap;

use miden_objects::account::AccountId;
use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
use miden_objects::crypto::hash::rpo::Rpo256;
use miden_objects::utils::Serializable;
use miden_objects::{Felt, FieldElement, Word};

/// Load the test account fixture from fixtures/account.json
/// Returns (account_id, account_id_hex, initial_state_json_value)
fn load_fixture_account() -> (AccountId, String, serde_json::Value) {
    let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("account.json");

    let fixture_contents = std::fs::read_to_string(&fixture_path)
        .expect("Failed to read fixture file - run fetch_fixture_account test first");

    let fixture_json: serde_json::Value = serde_json::from_str(&fixture_contents)
        .expect("Failed to parse fixture JSON");

    let account_id_hex = fixture_json["account_id"]
        .as_str()
        .expect("No account_id in fixture")
        .to_string();

    let account_id = AccountId::from_hex(&account_id_hex).expect("Invalid account ID in fixture");

    // Return the fixture JSON value (not string) for HTTP tests
    (account_id, account_id_hex, fixture_json)
}

/// Helper to get just the test account ID (for old tests that use invalid JSON)
fn get_test_account_id() -> (AccountId, String) {
    let account_id_hex = "0x8a65fc5a39e4cd106d648e3eb4ab5f";
    let account_id = AccountId::from_hex(account_id_hex).expect("Valid account ID");
    (account_id, account_id_hex.to_string())
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
    let message: Word = digest;

    // Sign the message
    let signature = secret_key.sign(message);

    // Convert to hex strings
    let pubkey_word: Word = public_key.into();
    let pubkey_hex = format!("0x{}", hex::encode(pubkey_word.to_bytes()));
    let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

    (account_id_hex.to_string(), pubkey_hex, signature_hex)
}

/// Helper to create test app state
async fn create_test_app_state() -> AppState {
    // Create temporary directory for test storage
    let storage_dir =
        std::env::temp_dir().join(format!("psm_test_storage_{}", uuid::Uuid::new_v4()));
    let metadata_dir =
        std::env::temp_dir().join(format!("psm_test_metadata_{}", uuid::Uuid::new_v4()));

    std::fs::create_dir_all(&storage_dir).expect("Failed to create storage directory");
    std::fs::create_dir_all(&metadata_dir).expect("Failed to create metadata directory");

    let storage = FilesystemService::new(storage_dir)
        .await
        .expect("Failed to create storage");
    let metadata = FilesystemMetadataStore::new(metadata_dir)
        .await
        .expect("Failed to create metadata");

    // Create storage registry
    let mut storage_backends: HashMap<StorageType, Arc<dyn StorageBackend>> = HashMap::new();
    storage_backends.insert(StorageType::Filesystem, Arc::new(storage));
    let storage_registry = StorageRegistry::new(storage_backends);

    // Create network client
    let network_client = server::network::miden::MidenNetworkClient::from_network(NetworkType::MidenTestnet)
        .await
        .expect("Failed to create network client");

    AppState {
        storage: storage_registry,
        metadata: Arc::new(metadata),
        network_type: NetworkType::MidenTestnet,
        network_client: Arc::new(tokio::sync::Mutex::new(network_client)),
    }
}

/// Helper to create the router
fn create_router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/configure", axum::routing::post(http::configure))
        .route("/push_delta", axum::routing::post(http::push_delta))
        .route("/get_delta", axum::routing::get(http::get_delta))
        .route("/get_delta_head", axum::routing::get(http::get_delta_head))
        .route("/get_state", axum::routing::get(http::get_state))
        .with_state(state)
}

#[tokio::test]
async fn test_configure_account() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();

    // Prepare configure request
    let request_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_pubkeys": []
            }
        },
        "initial_state": initial_state,
        "storage_type": "Filesystem"
    });

    let request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&request_body).unwrap()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    let status = response.status();
    // Print response body if not OK for debugging
    if status != StatusCode::OK {
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
        println!("Response status: {}", status);
        println!("Response body: {}", body_str);
    }

    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn test_configure_and_push_delta_with_auth() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let (_, pubkey_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Step 1: Configure account with the cosigner public key
    let configure_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_pubkeys": [pubkey_hex.clone()]
            }
        },
        "initial_state": initial_state,
        "storage_type": "Filesystem"
    });

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&configure_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let configure_response = app_clone.call(configure_request).await.unwrap();

    assert_eq!(
        configure_response.status(),
        StatusCode::OK,
        "Configure should succeed"
    );

    // Step 2: Push a delta with authentication headers
    let delta_body = json!({
        "account_id": account_id_hex,
        "nonce": 1,
        "prev_commitment": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "delta_hash": "0x1111111111111111111111111111111111111111111111111111111111111111",
        "delta_payload": {
            "changes": ["balance_update"]
        },
        "ack_sig": "",
        "candidate_at": "2024-01-01T00:00:00Z"
    });

    let push_request = Request::builder()
        .uri("/push_delta")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", pubkey_hex)
        .header("x-signature", signature_hex)
        .body(Body::from(serde_json::to_string(&delta_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let push_response = app_clone.call(push_request).await.unwrap();

    assert_eq!(
        push_response.status(),
        StatusCode::OK,
        "Push delta should succeed with valid auth"
    );
}

#[tokio::test]
async fn test_push_delta_unauthorized_cosigner() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();

    // Generate two different key pairs
    let (_, authorized_pubkey, _) = generate_falcon_signature(&account_id_hex);
    let (_, unauthorized_pubkey, unauthorized_sig) = generate_falcon_signature(&account_id_hex);

    // Configure account with ONLY the authorized pubkey
    let configure_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_pubkeys": [authorized_pubkey] // Only this key is authorized
            }
        },
        "initial_state": initial_state,
        "storage_type": "Filesystem"
    });

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&configure_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let configure_response = app_clone.call(configure_request).await.unwrap();

    assert_eq!(configure_response.status(), StatusCode::OK);

    // Try to push delta with UNAUTHORIZED key
    let delta_body = json!({
        "account_id": account_id_hex,
        "nonce": 1,
        "prev_commitment": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "delta_hash": "0x1111111111111111111111111111111111111111111111111111111111111111",
        "delta_payload": {
            "changes": ["balance_update"]
        },
        "ack_sig": "",
        "candidate_at": "2024-01-01T00:00:00Z"
    });

    let push_request = Request::builder()
        .uri("/push_delta")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", unauthorized_pubkey)
        .header("x-signature", unauthorized_sig)
        .body(Body::from(serde_json::to_string(&delta_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let push_response = app_clone.call(push_request).await.unwrap();

    // Should fail because the public key is not in cosigner_pubkeys list
    assert_eq!(
        push_response.status(),
        StatusCode::BAD_REQUEST,
        "Should reject unauthorized cosigner"
    );
}

#[tokio::test]
async fn test_push_delta_missing_auth_headers() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let (_, pubkey_hex, _) = generate_falcon_signature(&account_id_hex);

    // Configure account
    let configure_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_pubkeys": [pubkey_hex]
            }
        },
        "initial_state": initial_state,
        "storage_type": "Filesystem"
    });

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&configure_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let configure_response = app_clone.call(configure_request).await.unwrap();

    assert_eq!(configure_response.status(), StatusCode::OK);

    // Try to push delta WITHOUT auth headers
    let delta_body = json!({
        "account_id": account_id_hex,
        "nonce": 1,
        "prev_commitment": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "delta_hash": "0x1111111111111111111111111111111111111111111111111111111111111111",
        "delta_payload": {
            "changes": ["balance_update"]
        },
        "ack_sig": "",
        "candidate_at": "2024-01-01T00:00:00Z"
    });

    let push_request = Request::builder()
        .uri("/push_delta")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        // NO auth headers!
        .body(Body::from(serde_json::to_string(&delta_body).unwrap()))
        .unwrap();

    let push_response = app.oneshot(push_request).await.unwrap();

    // Should fail with BAD_REQUEST because auth headers are missing
    assert_eq!(
        push_response.status(),
        StatusCode::BAD_REQUEST,
        "Should require auth headers"
    );
}
