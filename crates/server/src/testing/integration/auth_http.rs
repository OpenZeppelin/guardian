use crate::testing::helpers::{
    TestSigner, create_router, create_test_app_state, load_fixture_account, load_fixture_delta,
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
    let signer = TestSigner::new();

    // Step 1: Configure account with the cosigner commitment
    let configure_body = json!({
        "account_id": account_id_hex.clone(),
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer.commitment_hex]
            }
        },
        "initial_state": initial_state
    });
    let (signature_hex, timestamp) = signer.sign_json_payload(&account_id_hex, &configure_body);

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex)
        .header("x-timestamp", timestamp.to_string())
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
    let (signature_hex_2, timestamp_2) = signer.sign_json_payload(&account_id_hex, &delta_body);

    let push_request = Request::builder()
        .uri("/push_delta")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", signature_hex_2)
        .header("x-timestamp", timestamp_2.to_string())
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
    let authorized_signer = TestSigner::new();
    let unauthorized_signer = TestSigner::new();

    // Configure account with ONLY the authorized commitment
    let configure_body = json!({
        "account_id": account_id_hex.clone(),
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [authorized_signer.commitment_hex] // Only this commitment is authorized
            }
        },
        "initial_state": initial_state
    });
    let (authorized_sig, authorized_ts) =
        authorized_signer.sign_json_payload(&account_id_hex, &configure_body);

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &authorized_signer.pubkey_hex)
        .header("x-signature", &authorized_sig)
        .header("x-timestamp", authorized_ts.to_string())
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
    let (unauthorized_sig, unauthorized_ts) =
        unauthorized_signer.sign_json_payload(&account_id_hex, &delta_body);

    let push_request = Request::builder()
        .uri("/push_delta")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &unauthorized_signer.pubkey_hex)
        .header("x-signature", unauthorized_sig)
        .header("x-timestamp", unauthorized_ts.to_string())
        .body(Body::from(serde_json::to_string(&delta_body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let push_response = app_clone.call(push_request).await.unwrap();

    // Should fail because the public key commitment is not in authorized commitments list
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
    let signer = TestSigner::new();

    // Configure account
    let configure_body = json!({
        "account_id": account_id_hex.clone(),
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer.commitment_hex]
            }
        },
        "initial_state": initial_state
    });
    let (signature_hex, timestamp) = signer.sign_json_payload(&account_id_hex, &configure_body);

    let configure_request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex)
        .header("x-timestamp", timestamp.to_string())
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
