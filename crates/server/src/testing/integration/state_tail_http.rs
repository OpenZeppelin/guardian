//! HTTP integration tests for `GET /state/tail` (issue #517): the queue tail
//! a cosigner builds on, replayed with the real Miden delta path.

use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::state::AppState;
use crate::testing::fixtures;
use crate::testing::helpers::{
    IntegrationMockNetworkClient, TestSigner, create_router, create_test_app_state, fixture_signer,
    load_fixture_account, load_queue_fixture_delta,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::json;
use std::sync::Arc;
use tower::Service;

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
        "auth": { "MidenFalconRpo": { "cosigner_commitments": cosigner_commitments } },
        "initial_state": initial_state
    });
    let (signature_hex, timestamp) = signer.sign_json_payload(&account_id_hex, &configure_body);
    let response = app
        .clone()
        .call(
            Request::builder()
                .uri("/configure")
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-pubkey", &signer.pubkey_hex)
                .header("x-signature", &signature_hex)
                .header("x-timestamp", timestamp.to_string())
                .body(Body::from(configure_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "configure succeeds");

    (app, state, signer, account_id_hex)
}

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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!(null)),
    )
}

/// `queue_1` stored as a queued candidate on the registered state, as an
/// acknowledged push leaves it.
async fn queue_first_candidate(state: &AppState, account_id: &str) -> String {
    let fixture = load_queue_fixture_delta(1);
    let commitments: serde_json::Value =
        serde_json::from_str(fixtures::COMMITMENTS_JSON).expect("commitments parse");
    let new_commitment = commitments["commitment_after_queue_1"]
        .as_str()
        .expect("pinned commitment")
        .to_string();
    state
        .storage
        .submit_delta(&DeltaObject {
            account_id: account_id.to_string(),
            nonce: fixture["nonce"].as_u64().expect("nonce"),
            prev_commitment: fixture["prev_commitment"]
                .as_str()
                .expect("prev")
                .to_string(),
            new_commitment: Some(new_commitment.clone()),
            delta_payload: fixture["delta_payload"].clone(),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::candidate("2026-11-11T10:00:00Z".to_string()),
            metadata: None,
        })
        .await
        .expect("candidate stored");
    new_commitment
}

#[tokio::test]
async fn test_state_tail_is_the_canonical_state_with_nothing_queued() {
    let (app, _state, signer, account_id_hex) = configured_account().await;

    let (status, tail) = signed_get(&app, &signer, "/state/tail", &account_id_hex).await;
    assert_eq!(status, StatusCode::OK, "body: {tail}");
    let (_, canonical) = signed_get(&app, &signer, "/state", &account_id_hex).await;

    assert_eq!(
        tail["state"], canonical,
        "served exactly as GET /state serves it"
    );
    assert_eq!(tail["queue"], json!([]));
    assert_eq!(tail["accepts_submissions"], json!(true));
    let mut keys: Vec<_> = tail.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["accepts_submissions", "queue", "state"]);
}

#[tokio::test]
async fn test_state_tail_replays_a_queued_candidate() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let canonical_commitment = state
        .storage
        .pull_state(&account_id_hex)
        .await
        .expect("configured")
        .commitment;
    let tail_commitment = queue_first_candidate(&state, &account_id_hex).await;

    let (status, tail) = signed_get(&app, &signer, "/state/tail", &account_id_hex).await;
    assert_eq!(status, StatusCode::OK, "body: {tail}");
    assert_eq!(tail["state"]["commitment"], tail_commitment);
    assert_eq!(tail["state"]["account_id"], account_id_hex);
    assert_eq!(tail["state"]["updated_at"], "2026-11-11T10:00:00Z");
    assert!(tail["state"]["state_json"]["data"].is_string());
    assert!(
        tail["state"].get("nonce").is_none(),
        "the stored nonce stays server-internal"
    );
    assert_eq!(
        tail["queue"],
        json!([{
            "nonce": 1,
            "prev_commitment": canonical_commitment,
            "new_commitment": tail_commitment,
        }])
    );
    assert_eq!(
        tail["accepts_submissions"],
        json!(false),
        "at the default depth of 1 the queued candidate fills the queue"
    );

    let (_, canonical) = signed_get(&app, &signer, "/state", &account_id_hex).await;
    assert_eq!(
        canonical["commitment"], canonical_commitment,
        "GET /state keeps serving the canonical state"
    );
}

/// With room behind the queued candidate, which keeps the signers, the tail
/// takes a submission.
#[tokio::test]
async fn test_state_tail_with_room_accepts_submissions() {
    let (_, mut state, signer, account_id_hex) = configured_account().await;
    state.canonicalization = Some(
        crate::canonicalization::CanonicalizationConfig::default()
            .with_max_pending_candidates_per_account(2),
    );
    let app = create_router(state.clone());
    let tail_commitment = queue_first_candidate(&state, &account_id_hex).await;

    let (status, tail) = signed_get(&app, &signer, "/state/tail", &account_id_hex).await;
    assert_eq!(status, StatusCode::OK, "body: {tail}");
    assert_eq!(tail["state"]["commitment"], tail_commitment);
    assert_eq!(tail["accepts_submissions"], json!(true));
}

#[tokio::test]
async fn test_state_tail_unknown_account_is_not_found() {
    let (app, _state, signer, _account_id_hex) = configured_account().await;

    let unknown = "0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c";
    let (status, body) = signed_get(&app, &signer, "/state/tail", unknown).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "account_not_found");
}

#[tokio::test]
async fn test_state_tail_refuses_an_unsigned_or_foreign_request() {
    let (app, _state, _signer, account_id_hex) = configured_account().await;

    let response = app
        .clone()
        .call(
            Request::builder()
                .uri(format!("/state/tail?account_id={account_id_hex}"))
                .method("GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "unsigned");

    let (status, body) = signed_get(&app, &TestSigner::new(), "/state/tail", &account_id_hex).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
}

/// The signature covers the query: one made over another account's query does
/// not authenticate a read of this one.
#[tokio::test]
async fn test_state_tail_signature_covers_the_account() {
    let (app, _state, signer, account_id_hex) = configured_account().await;
    let other = "0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c";
    let payload = json!({ "account_id": other });
    let (signature_hex, timestamp) = signer.sign_json_payload(&account_id_hex, &payload);

    let response = app
        .clone()
        .call(
            Request::builder()
                .uri(format!("/state/tail?account_id={account_id_hex}"))
                .method("GET")
                .header("x-pubkey", &signer.pubkey_hex)
                .header("x-signature", &signature_hex)
                .header("x-timestamp", timestamp.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
