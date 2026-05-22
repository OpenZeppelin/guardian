//! Error-envelope contract tests for HTTP handlers.
//!
//! Locks down the wire shape returned by every handler that funnels errors
//! through `GuardianError → IntoResponse`. The legacy anti-pattern (status 400
//! with the error message stuffed into `delta.account_id`) is silently
//! reintroduced if a handler returns `(StatusCode, Json<DomainObject>)`; this
//! test would fail in that case because the response body would not match the
//! `ErrorResponse` envelope below.

use crate::testing::helpers::{
    TestSigner, create_router, create_test_app_state, load_fixture_account, load_fixture_delta,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use tower::Service;

#[derive(Deserialize, Debug)]
struct ErrorEnvelope {
    success: bool,
    code: String,
    error: String,
}

async fn parse_envelope(response: axum::http::Response<Body>) -> ErrorEnvelope {
    let status = response.status();
    let body_bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|e| {
        panic!(
            "expected JSON body, got: {} ({})",
            String::from_utf8_lossy(&body_bytes),
            e
        )
    });

    let obj = value.as_object().expect("body must be a JSON object");
    assert!(
        !obj.contains_key("delta"),
        "error body must not be a domain object; got: {value}"
    );
    assert!(
        !obj.contains_key("nonce"),
        "error body must not be a delta object; got: {value}"
    );

    let envelope: ErrorEnvelope = serde_json::from_value(value.clone())
        .unwrap_or_else(|e| panic!("body does not match ErrorEnvelope ({e}): {value}"));
    assert!(!envelope.success, "success must be false ({status})");
    assert!(!envelope.code.is_empty(), "code must be non-empty");
    assert!(!envelope.error.is_empty(), "error must be non-empty");
    envelope
}

#[tokio::test]
async fn push_delta_proposal_error_uses_error_envelope() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let (_account_id, account_id_hex, _initial_state) = load_fixture_account();
    let signer = TestSigner::new();

    let body = json!({
        "account_id": account_id_hex,
        "nonce": 1,
        "delta_payload": { "tx_summary": {}, "signatures": [], "metadata": {} }
    });

    let request = Request::builder()
        .uri("/push_delta_proposal")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", format!("0x{}", "ab".repeat(666)))
        .header("x-timestamp", "0")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let response = app_clone.call(request).await.unwrap();
    assert!(
        !response.status().is_success(),
        "status should be non-2xx for bad signature"
    );
    let envelope = parse_envelope(response).await;
    // Bad signature must surface as a real error code, not "everything is 400".
    assert_ne!(
        envelope.code, "INTERNAL_ERROR",
        "bad signature should map to a recognized error variant, got: {envelope:?}"
    );
}

#[tokio::test]
async fn push_delta_error_uses_error_envelope() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let signer = TestSigner::new();
    let body = json!({ "account_id": "0xnope", "nonce": 1 });

    let request = Request::builder()
        .uri("/push_delta")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", format!("0x{}", "ab".repeat(666)))
        .header("x-timestamp", "0")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let response = app_clone.call(request).await.unwrap();
    assert!(!response.status().is_success());
    parse_envelope(response).await;
}

#[tokio::test]
async fn get_delta_error_uses_error_envelope() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let signer = TestSigner::new();
    let (_account_id, account_id_hex, _initial_state) = load_fixture_account();
    let uri = format!("/get_delta?account_id={account_id_hex}&nonce=1");

    let request = Request::builder()
        .uri(uri)
        .method("GET")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", format!("0x{}", "ab".repeat(666)))
        .header("x-timestamp", "0")
        .body(Body::empty())
        .unwrap();

    let mut app_clone = app.clone();
    let response = app_clone.call(request).await.unwrap();
    assert!(!response.status().is_success());
    parse_envelope(response).await;
}

#[tokio::test]
async fn get_state_error_uses_error_envelope() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let signer = TestSigner::new();
    let (_account_id, account_id_hex, _initial_state) = load_fixture_account();
    let uri = format!("/get_state?account_id={account_id_hex}");

    let request = Request::builder()
        .uri(uri)
        .method("GET")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", format!("0x{}", "ab".repeat(666)))
        .header("x-timestamp", "0")
        .body(Body::empty())
        .unwrap();

    let mut app_clone = app.clone();
    let response = app_clone.call(request).await.unwrap();
    assert!(!response.status().is_success());
    parse_envelope(response).await;
}

#[tokio::test]
async fn get_delta_proposal_error_uses_error_envelope() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let signer = TestSigner::new();
    let (_account_id, account_id_hex, _initial_state) = load_fixture_account();
    let uri = format!("/get_delta_proposal?account_id={account_id_hex}&commitment=0xmissing");

    let request = Request::builder()
        .uri(uri)
        .method("GET")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", format!("0x{}", "ab".repeat(666)))
        .header("x-timestamp", "0")
        .body(Body::empty())
        .unwrap();

    let mut app_clone = app.clone();
    let response = app_clone.call(request).await.unwrap();
    assert!(!response.status().is_success());
    parse_envelope(response).await;
}

#[tokio::test]
async fn push_delta_proposal_on_paused_account_returns_409_envelope() {
    let state = create_test_app_state().await;
    let app = create_router(state.clone());

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();

    // Configure the account so it exists in the metadata store.
    let configure_body = json!({
        "account_id": account_id_hex,
        "auth": {
            "MidenFalconRpo": {
                "cosigner_commitments": [signer.commitment_hex.clone()]
            }
        },
        "initial_state": initial_state
    });
    let (sig, ts) = signer.sign_json_payload(&account_id_hex, &configure_body);
    let configure_req = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &sig)
        .header("x-timestamp", ts.to_string())
        .body(Body::from(serde_json::to_string(&configure_body).unwrap()))
        .unwrap();
    let mut app_clone = app.clone();
    let configure_resp = app_clone.call(configure_req).await.unwrap();
    assert_eq!(configure_resp.status(), StatusCode::OK);

    // Pause the account directly through the metadata store. This bypasses
    // the dashboard pause endpoint (separate auth surface) and exercises the
    // pause chokepoint via `push_delta_proposal`.
    let pause_reason = "compliance review";
    state
        .metadata
        .set_pause(&account_id_hex, Utc::now(), pause_reason)
        .await
        .expect("set_pause should succeed");

    // Attempt to push a proposal against the paused account.
    let delta_1 = load_fixture_delta(1);
    let proposal_body = json!({
        "account_id": account_id_hex,
        "nonce": 1,
        "delta_payload": {
            "tx_summary": delta_1["delta_payload"],
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [signer.commitment_hex.clone()]
            }
        }
    });
    let (sig2, ts2) = signer.sign_json_payload(&account_id_hex, &proposal_body);
    let push_req = Request::builder()
        .uri("/push_delta_proposal")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &sig2)
        .header("x-timestamp", ts2.to_string())
        .body(Body::from(serde_json::to_string(&proposal_body).unwrap()))
        .unwrap();
    let mut app_clone = app.clone();
    let push_resp = app_clone.call(push_req).await.unwrap();

    assert_eq!(
        push_resp.status(),
        StatusCode::CONFLICT,
        "push_delta_proposal on a paused account must return 409"
    );

    let body_bytes = to_bytes(push_resp.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|e| {
        panic!(
            "expected JSON body, got: {} ({})",
            String::from_utf8_lossy(&body_bytes),
            e
        )
    });
    let obj = value.as_object().expect("body must be a JSON object");
    assert!(
        !obj.contains_key("delta"),
        "paused-account error body must not be shaped like a domain object; got: {value}"
    );

    let envelope: ErrorEnvelope = serde_json::from_value(value.clone())
        .unwrap_or_else(|e| panic!("body does not match ErrorEnvelope ({e}): {value}"));
    assert!(!envelope.success);
    assert_eq!(envelope.code, "GUARDIAN_ACCOUNT_PAUSED");
    assert!(envelope.error.contains(pause_reason));

    let paused_at = obj
        .get("paused_at")
        .and_then(|v| v.as_str())
        .expect("paused_at must be a string");
    assert!(!paused_at.is_empty(), "paused_at must be non-empty");
    let paused_reason = obj
        .get("paused_reason")
        .and_then(|v| v.as_str())
        .expect("paused_reason must be a string");
    assert_eq!(paused_reason, pause_reason);
}

#[tokio::test]
async fn get_delta_proposals_error_uses_error_envelope() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let signer = TestSigner::new();
    let (_account_id, account_id_hex, _initial_state) = load_fixture_account();
    let uri = format!("/get_delta_proposals?account_id={account_id_hex}");

    let request = Request::builder()
        .uri(uri)
        .method("GET")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", format!("0x{}", "ab".repeat(666)))
        .header("x-timestamp", "0")
        .body(Body::empty())
        .unwrap();

    let mut app_clone = app.clone();
    let response = app_clone.call(request).await.unwrap();
    // Previously this endpoint swallowed errors as 200 { proposals: [] }.
    assert!(
        !response.status().is_success(),
        "get_delta_proposals must not return 2xx on auth failure"
    );
    parse_envelope(response).await;
}

#[tokio::test]
async fn sign_delta_proposal_error_uses_error_envelope() {
    let state = create_test_app_state().await;
    let app = create_router(state);

    let signer = TestSigner::new();
    let (_account_id, account_id_hex, _initial_state) = load_fixture_account();
    let body = json!({
        "account_id": account_id_hex,
        "commitment": "0xmissing",
        "signature": { "scheme": "falcon", "signature": format!("0x{}", "a".repeat(666)) }
    });

    let request = Request::builder()
        .uri("/sign_delta_proposal")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", format!("0x{}", "ab".repeat(666)))
        .header("x-timestamp", "0")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let mut app_clone = app.clone();
    let response = app_clone.call(request).await.unwrap();
    assert!(!response.status().is_success());
    parse_envelope(response).await;
}
