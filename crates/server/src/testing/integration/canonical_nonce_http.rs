//! HTTP integration tests for `GET /state/nonce` (issue #191): the
//! canonical-nonce pre-check clients run before `GET /state`, served from
//! the nonce and commitment stored with the state.

use crate::jobs::canonicalization::process_canonicalizations_now;
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::state::AppState;
use crate::state_object::StateHead;
use crate::testing::helpers::{
    IntegrationMockNetworkClient, TestSigner, create_router, create_test_app_state, fixture_signer,
    load_fixture_account, load_fixture_delta,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use guardian_shared::FromJson;
use miden_protocol::account::Account;
use serde_json::json;
use std::sync::Arc;
use tower::Service;

/// The fixture account configured through the real Miden decoder, so the
/// nonce stored with the state, and served by the endpoint, is read from
/// the account itself rather than from a mock.
async fn configured_account() -> (axum::Router, AppState, TestSigner, String) {
    let mut state = create_test_app_state().await;
    state.network_client = Arc::new(IntegrationMockNetworkClient::new(
        MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal),
    ));
    let app = create_router(state.clone());

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let (signer, cosigner_commitments) = fixture_signer();

    let configure_body = json!({
        "account_id": account_id_hex.clone(),
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": cosigner_commitments
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

    (app, state, signer, account_id_hex)
}

/// The nonce a decode of `state_json` reads.
fn decoded_nonce(state_json: &serde_json::Value) -> u64 {
    Account::from_json(state_json)
        .expect("stored state decodes")
        .nonce()
        .as_canonical_u64()
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
    let (app, _state, signer, account_id_hex) = configured_account().await;

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
    let (app, _state, signer, account_id_hex) = configured_account().await;

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
    let (app, _state, signer, _account_id_hex) = configured_account().await;

    let unknown = "0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c";
    let (status, body) = signed_get(&app, &signer, "/state/nonce", unknown).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "account_not_found");
}

/// Configure stores the nonce a decode of the stored state reads, and the
/// endpoint serves that stored head.
#[tokio::test]
async fn test_configure_stores_the_decoded_nonce() {
    let (app, state, signer, account_id_hex) = configured_account().await;

    let stored = state
        .storage
        .pull_state(&account_id_hex)
        .await
        .expect("configured state");
    let decoded = decoded_nonce(&stored.state_json);
    assert_eq!(stored.nonce, Some(decoded));

    let (status, body) = signed_get(&app, &signer, "/state/nonce", &account_id_hex).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["nonce"], decoded);
    assert_eq!(body["commitment"], stored.commitment);
}

/// A state row written before the server stored nonces is decoded on its
/// first read, and the nonce is stored for the reads after it.
#[tokio::test]
async fn test_a_state_without_a_stored_nonce_is_backfilled_on_first_read() {
    let (app, state, signer, account_id_hex) = configured_account().await;

    let mut legacy = state
        .storage
        .pull_state(&account_id_hex)
        .await
        .expect("configured state");
    let decoded = legacy.nonce.expect("configure stores a nonce");
    legacy.nonce = None;
    state.storage.submit_state(&legacy).await.expect("rewrite");
    assert_eq!(
        state
            .storage
            .pull_state_head(&account_id_hex)
            .await
            .unwrap(),
        StateHead {
            commitment: legacy.commitment.clone(),
            nonce: None,
        }
    );

    let (status, body) = signed_get(&app, &signer, "/state/nonce", &account_id_hex).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["nonce"], decoded);
    assert_eq!(body["commitment"], legacy.commitment);
    assert_eq!(
        state
            .storage
            .pull_state_head(&account_id_hex)
            .await
            .unwrap(),
        StateHead {
            commitment: legacy.commitment,
            nonce: Some(decoded),
        },
        "the first read stores the decoded nonce"
    );
}

/// Canonicalizing a delta stores the nonce of the state `apply_delta`
/// built, which is the nonce a decode of the stored state reads.
#[tokio::test]
async fn test_a_canonicalized_delta_stores_the_applied_nonce() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let configured = state
        .storage
        .pull_state_head(&account_id_hex)
        .await
        .expect("configured head");

    // The fixture account is a multisig, so the push gate needs the
    // matching threshold-satisfying proposal stored first.
    let delta_1 = load_fixture_delta(1);
    crate::testing::helpers::store_fixture_authorizing_proposal(&state, &delta_1).await;
    let delta_body = json!({
        "account_id": delta_1["account_id"],
        "nonce": delta_1["nonce"],
        "prev_commitment": delta_1["prev_commitment"],
        "delta_payload": delta_1["delta_payload"]
    });
    let (signature_hex, timestamp) = signer.sign_json_payload(&account_id_hex, &delta_body);
    let push = Request::builder()
        .uri("/delta")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature_hex)
        .header("x-timestamp", timestamp.to_string())
        .body(Body::from(serde_json::to_string(&delta_body).unwrap()))
        .unwrap();
    let response = app.clone().call(push).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "push delta");

    process_canonicalizations_now(&state)
        .await
        .expect("canonicalization pass");

    let stored = state
        .storage
        .pull_state(&account_id_hex)
        .await
        .expect("canonical state");
    assert_eq!(stored.commitment, delta_1["new_commitment"]);
    assert_eq!(stored.nonce, Some(decoded_nonce(&stored.state_json)));
    assert!(
        stored.nonce > configured.nonce,
        "the canonicalized state is past the configured one"
    );

    let (status, body) = signed_get(&app, &signer, "/state/nonce", &account_id_hex).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["nonce"], stored.nonce.unwrap());
    assert_eq!(body["commitment"], stored.commitment);
}
