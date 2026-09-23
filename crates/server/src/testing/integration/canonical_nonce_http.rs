//! HTTP integration tests for `GET /state/nonce` (issue #191): the
//! lightweight canonical-nonce pre-check clients run before `GET /state`.

use crate::testing::helpers::{
    TestSigner, create_router, create_test_app_state, load_fixture_account,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use guardian_shared::FromJson;
use miden_protocol::account::Account;
use serde_json::json;
use tower::Service;

async fn configured_account() -> (axum::Router, TestSigner, String) {
    let state = create_test_app_state().await;
    let app = create_router(state.clone());

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();

    let configure_body = json!({
        "account_id": account_id_hex.clone(),
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer.commitment_hex.clone()]
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
    let response = app.clone().call(configure_request).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "Configure should succeed"
    );

    (app, signer, account_id_hex)
}

/// Signed GET against a `/state*` route whose only query is `account_id`.
async fn signed_get(
    app: &axum::Router,
    signer: &TestSigner,
    path: &str,
    account_id: &str,
) -> (StatusCode, serde_json::Value) {
    let payload = json!({ "account_id": account_id });
    let (signature_hex, timestamp) = signer.sign_json_payload(account_id, &payload);

    let request = Request::builder()
        .uri(format!("{path}?account_id={account_id}"))
        .method("GET")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex)
        .header("x-timestamp", timestamp.to_string())
        .body(Body::empty())
        .unwrap();
    let response = app.clone().call(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(json!(null));
    (status, body)
}

#[tokio::test]
async fn test_canonical_nonce_matches_the_registered_state() {
    let (app, signer, account_id_hex) = configured_account().await;

    let (status, body) = signed_get(&app, &signer, "/state/nonce", &account_id_hex).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["account_id"], account_id_hex);
    assert!(body["nonce"].is_u64(), "nonce should be an integer: {body}");

    // The head matches what the full state fetch reports, without the blob.
    let (state_status, state_body) = signed_get(&app, &signer, "/state", &account_id_hex).await;
    assert_eq!(state_status, StatusCode::OK);
    assert_eq!(body["commitment"], state_body["commitment"]);
    assert!(
        body.get("state_json").is_none(),
        "nonce response must not carry the state blob"
    );

    let (_, _, initial_state) = load_fixture_account();
    let expected_nonce = Account::from_json(&initial_state)
        .expect("fixture decodes")
        .nonce()
        .as_canonical_u64();
    assert_eq!(body["nonce"], expected_nonce);
}

#[tokio::test]
async fn test_canonical_nonce_rejects_a_signature_over_a_different_query() {
    let (app, signer, account_id_hex) = configured_account().await;

    let other_payload = json!({ "account_id": "0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c" });
    let (signature_hex, timestamp) = signer.sign_json_payload(&account_id_hex, &other_payload);
    let request = Request::builder()
        .uri(format!("/state/nonce?account_id={account_id_hex}"))
        .method("GET")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex)
        .header("x-timestamp", timestamp.to_string())
        .body(Body::empty())
        .unwrap();
    let response = app.clone().call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_canonical_nonce_unknown_account_is_not_found() {
    let (app, signer, _account_id_hex) = configured_account().await;

    let unknown = "0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c";
    let (status, body) = signed_get(&app, &signer, "/state/nonce", unknown).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "account_not_found");
}
