use crate::testing::helpers::*;
use crate::testing::helpers::{
    load_fixture_account_grpc as load_fixture_account, load_fixture_delta,
};

use crate::api::grpc::state_manager::state_manager_server::StateManager;
use crate::api::grpc::state_manager::{ConfigureRequest, GetDeltaRequest, PushDeltaRequest};

#[tokio::test]
async fn test_grpc_configure_and_push_delta_with_auth() {
    let state = create_test_app_state().await;
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let (pubkey_hex, commitment_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Step 1: Configure account with the cosigner commitment
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![commitment_hex.clone()])),
        initial_state,
    };

    let configure_response = service
        .configure(create_request_with_auth(
            configure_req,
            &pubkey_hex,
            &signature_hex,
        ))
        .await;
    assert!(configure_response.is_ok(), "Configure should succeed");
    assert!(configure_response.unwrap().into_inner().success);

    // Step 2: Push a delta with authentication metadata
    let delta_1 = load_fixture_delta(1);
    let push_req = PushDeltaRequest {
        account_id: delta_1["account_id"].as_str().unwrap().to_string(),
        nonce: delta_1["nonce"].as_u64().unwrap(),
        prev_commitment: delta_1["prev_commitment"].as_str().unwrap().to_string(),
        delta_payload: serde_json::to_string(&delta_1["delta_payload"]).unwrap(),
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
    let (authorized_pubkey, authorized_commitment, authorized_sig) =
        generate_falcon_signature(&account_id_hex);
    let (unauthorized_pubkey, _, unauthorized_sig) = generate_falcon_signature(&account_id_hex);

    // Configure account with ONLY the authorized commitment
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![authorized_commitment])), // Only this key is authorized
        initial_state,
    };

    let configure_response = service
        .configure(create_request_with_auth(
            configure_req,
            &authorized_pubkey,
            &authorized_sig,
        ))
        .await;
    assert!(configure_response.is_ok());
    assert!(configure_response.unwrap().into_inner().success);

    // Try to push delta with UNAUTHORIZED key
    let delta_1 = load_fixture_delta(1);
    let push_req = PushDeltaRequest {
        account_id: delta_1["account_id"].as_str().unwrap().to_string(),
        nonce: delta_1["nonce"].as_u64().unwrap(),
        prev_commitment: delta_1["prev_commitment"].as_str().unwrap().to_string(),
        delta_payload: serde_json::to_string(&delta_1["delta_payload"]).unwrap(),
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
    let (pubkey_hex, commitment_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Configure account
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![commitment_hex])),
        initial_state,
    };

    let configure_response = service
        .configure(create_request_with_auth(
            configure_req,
            &pubkey_hex,
            &signature_hex,
        ))
        .await;
    assert!(configure_response.is_ok());
    assert!(configure_response.unwrap().into_inner().success);

    // Try to push delta WITHOUT auth metadata
    let delta_1 = load_fixture_delta(1);
    let push_req = PushDeltaRequest {
        account_id: delta_1["account_id"].as_str().unwrap().to_string(),
        nonce: delta_1["nonce"].as_u64().unwrap(),
        prev_commitment: delta_1["prev_commitment"].as_str().unwrap().to_string(),
        delta_payload: serde_json::to_string(&delta_1["delta_payload"]).unwrap(),
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
    let (pubkey_hex, commitment_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Configure account
    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![commitment_hex.clone()])),
        initial_state,
    };

    service
        .configure(create_request_with_auth(
            configure_req,
            &pubkey_hex,
            &signature_hex,
        ))
        .await
        .unwrap();

    // Push a delta (nonce 1)
    let delta_1 = load_fixture_delta(1);
    let push_req = PushDeltaRequest {
        account_id: delta_1["account_id"].as_str().unwrap().to_string(),
        nonce: delta_1["nonce"].as_u64().unwrap(),
        prev_commitment: delta_1["prev_commitment"].as_str().unwrap().to_string(),
        delta_payload: serde_json::to_string(&delta_1["delta_payload"]).unwrap(),
    };

    service
        .push_delta(create_request_with_auth(
            push_req,
            &pubkey_hex,
            &signature_hex,
        ))
        .await
        .unwrap();

    // Get specific delta by nonce
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

    let delta = get_response.delta.unwrap();
    assert_eq!(delta.nonce, 1, "Delta should have nonce 1");
}
