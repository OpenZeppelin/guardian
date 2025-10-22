use crate::testing::helpers::{
    create_router, create_test_app_state, generate_falcon_signature, load_fixture_account,
    load_fixture_delta,
};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::json;
use tower::{Service, ServiceExt};

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

    let delta_1 = load_fixture_delta(1);
    let delta_body = json!({
        "account_id": delta_1["account_id"],
        "nonce": delta_1["nonce"],
        "prev_commitment": delta_1["prev_commitment"],
        "delta_payload": delta_1["delta_payload"]
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
    let delta_1 = load_fixture_delta(1);
    let delta_body = json!({
        "account_id": delta_1["account_id"],
        "nonce": delta_1["nonce"],
        "prev_commitment": delta_1["prev_commitment"],
        "delta_payload": delta_1["delta_payload"]
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
    let delta_1 = load_fixture_delta(1);
    let delta_body = json!({
        "account_id": delta_1["account_id"],
        "nonce": delta_1["nonce"],
        "prev_commitment": delta_1["prev_commitment"],
        "delta_payload": delta_1["delta_payload"]
    });

    let push_request = Request::builder()
        .uri("/push_delta")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        // NO auth headers!
        .body(Body::from(serde_json::to_string(&delta_body).unwrap()))
        .unwrap();

    let push_response = app.oneshot(push_request).await.unwrap();

    // Should fail with UNAUTHORIZED because auth headers are missing
    assert_eq!(
        push_response.status(),
        StatusCode::UNAUTHORIZED,
        "Should require auth headers"
    );
}
