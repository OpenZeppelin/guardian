mod utils;
use utils::test_helpers::*;

use server::api::grpc::state_manager::state_manager_server::StateManager;
use server::api::grpc::state_manager::{ConfigureRequest, GetDeltaRequest, PushDeltaRequest};
use utils::test_helpers::load_fixture_account_grpc as load_fixture_account;

#[tokio::test]
async fn test_grpc_configure_and_push_delta_with_auth() {
    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let (_, pubkey_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Step 1: Configure account with the cosigner public key
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![pubkey_hex.clone()])),
        initial_state,
        storage_type: "Filesystem".to_string(),
    };

    let configure_response = service.configure(Request::new(configure_req)).await;
    assert!(configure_response.is_ok(), "Configure should succeed");
    assert!(configure_response.unwrap().into_inner().success);

    // Step 2: Push a delta with authentication metadata
    let push_req = PushDeltaRequest {
        account_id: account_id_hex,
        nonce: 1,
        prev_commitment: "0x0000000000000000000000000000000000000000000000000000000000000000"
            .to_string(),
        delta_hash: "0x1111111111111111111111111111111111111111111111111111111111111111"
            .to_string(),
        delta_payload: r#"{"changes": ["balance_update"]}"#.to_string(),
        ack_sig: "".to_string(),
        candidate_at: "2024-01-01T00:00:00Z".to_string(),
        canonical_at: None,
        discarded_at: None,
    };

    let request = create_request_with_auth(push_req, &pubkey_hex, &signature_hex);
    let push_response = service.push_delta(request).await;

    assert!(
        push_response.is_ok(),
        "Push delta should succeed with valid auth"
    );
    let push_response = push_response.unwrap().into_inner();
    assert!(
        push_response.success,
        "Push response should be successful: {}",
        push_response.message
    );
}

#[tokio::test]
async fn test_grpc_push_delta_unauthorized_cosigner() {
    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();

    // Generate two different key pairs
    let (_, authorized_pubkey, _) = generate_falcon_signature(&account_id_hex);
    let (_, unauthorized_pubkey, unauthorized_sig) = generate_falcon_signature(&account_id_hex);

    // Configure account with ONLY the authorized pubkey
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![authorized_pubkey])), // Only this key is authorized
        initial_state,
        storage_type: "Filesystem".to_string(),
    };

    let configure_response = service.configure(Request::new(configure_req)).await;
    assert!(configure_response.is_ok());
    assert!(configure_response.unwrap().into_inner().success);

    // Try to push delta with UNAUTHORIZED key
    let push_req = PushDeltaRequest {
        account_id: account_id_hex,
        nonce: 1,
        prev_commitment: "0x0000000000000000000000000000000000000000000000000000000000000000"
            .to_string(),
        delta_hash: "0x1111111111111111111111111111111111111111111111111111111111111111"
            .to_string(),
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
    assert!(
        !push_response.success,
        "Push should fail with unauthorized cosigner"
    );
    assert!(
        push_response.message.contains("not authorized"),
        "Error message should mention authorization"
    );
}

#[tokio::test]
async fn test_grpc_push_delta_missing_auth_metadata() {
    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let (_, pubkey_hex, _) = generate_falcon_signature(&account_id_hex);

    // Configure account
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![pubkey_hex])),
        initial_state,
        storage_type: "Filesystem".to_string(),
    };

    let configure_response = service.configure(Request::new(configure_req)).await;
    assert!(configure_response.is_ok());
    assert!(configure_response.unwrap().into_inner().success);

    // Try to push delta WITHOUT auth metadata
    let push_req = PushDeltaRequest {
        account_id: account_id_hex,
        nonce: 1,
        prev_commitment: "0x0000000000000000000000000000000000000000000000000000000000000000"
            .to_string(),
        delta_hash: "0x1111111111111111111111111111111111111111111111111111111111111111"
            .to_string(),
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
    assert_eq!(
        error.code(),
        tonic::Code::InvalidArgument,
        "Should be InvalidArgument error"
    );
    assert!(
        error.message().contains("x-pubkey") || error.message().contains("x-signature"),
        "Error should mention missing metadata"
    );
}

#[tokio::test]
async fn test_grpc_get_delta_with_auth() {
    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let (_, pubkey_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Configure account
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![pubkey_hex.clone()])),
        initial_state,
        storage_type: "Filesystem".to_string(),
    };

    service
        .configure(Request::new(configure_req))
        .await
        .unwrap();

    // Push a delta
    let push_req = PushDeltaRequest {
        account_id: account_id_hex.clone(),
        nonce: 1,
        prev_commitment: "0x0000000000000000000000000000000000000000000000000000000000000000"
            .to_string(),
        delta_hash: "0x1111111111111111111111111111111111111111111111111111111111111111"
            .to_string(),
        delta_payload: r#"{"changes": ["balance_update"]}"#.to_string(),
        ack_sig: "".to_string(),
        candidate_at: "2024-01-01T00:00:00Z".to_string(),
        canonical_at: None,
        discarded_at: None,
    };

    service
        .push_delta(create_request_with_auth(
            push_req,
            &pubkey_hex,
            &signature_hex,
        ))
        .await
        .unwrap();

    // Get delta with auth
    let get_req = GetDeltaRequest {
        account_id: account_id_hex,
        nonce: 1,
    };

    let request = create_request_with_auth(get_req, &pubkey_hex, &signature_hex);
    let get_response = service.get_delta(request).await;

    assert!(
        get_response.is_ok(),
        "Get delta should succeed with valid auth"
    );
    let get_response = get_response.unwrap().into_inner();
    assert!(get_response.success, "Get response should be successful");
    assert!(get_response.delta.is_some(), "Should return delta");
    assert_eq!(
        get_response.delta.unwrap().nonce,
        1,
        "Should return correct delta"
    );
}
