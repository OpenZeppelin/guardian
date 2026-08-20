//! HTTP integration tests for `GET /history` (issue #413): the
//! client-facing paginated canonical transaction history feed.

use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::testing::helpers::{
    TestSigner, create_router, create_test_app_state, load_fixture_account, load_fixture_delta,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::json;
use tower::Service;

/// Configure a fixture account through the real endpoint so metadata
/// and auth exist, then return the shared state, router, signer, and
/// account id.
async fn configured_account() -> (crate::state::AppState, axum::Router, TestSigner, String) {
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

    (state, app, signer, account_id_hex)
}

/// Seed one delta row directly into storage. The history feed reads
/// persisted rows, so tests seed lifecycle states directly instead of
/// driving the canonicalization worker.
async fn seed_delta(
    state: &crate::state::AppState,
    account_id: &str,
    nonce: u64,
    status: DeltaStatus,
    payload: serde_json::Value,
) {
    let delta = DeltaObject {
        account_id: account_id.to_string(),
        nonce,
        prev_commitment: format!("0xprev{nonce:04}"),
        new_commitment: Some(format!("0xnew{nonce:04}")),
        delta_payload: payload,
        ack_sig: String::new(),
        ack_pubkey: String::new(),
        ack_scheme: String::new(),
        status,
        metadata: None,
    };
    state
        .storage
        .submit_delta(&delta)
        .await
        .expect("seed delta");
}

fn canonical_at(nonce: u64) -> DeltaStatus {
    DeltaStatus::Canonical {
        timestamp: format!("2026-08-01T12:00:{:02}Z", nonce % 60),
    }
}

/// Signed GET /history call; returns (status, parsed body).
async fn get_history(
    app: &axum::Router,
    signer: &TestSigner,
    account_id: &str,
    limit: Option<&str>,
    cursor: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut payload = serde_json::Map::new();
    payload.insert("account_id".to_string(), json!(account_id));
    let mut query = format!("account_id={account_id}");
    if let Some(limit) = limit {
        payload.insert("limit".to_string(), json!(limit));
        query.push_str(&format!("&limit={limit}"));
    }
    if let Some(cursor) = cursor {
        payload.insert("cursor".to_string(), json!(cursor));
        query.push_str(&format!("&cursor={cursor}"));
    }
    let payload = serde_json::Value::Object(payload);
    let (signature_hex, timestamp) = signer.sign_json_payload(account_id, &payload);

    let request = Request::builder()
        .uri(format!("/history?{query}"))
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
async fn test_history_pagination_returns_canonical_only_newest_first() {
    let (state, app, signer, account_id_hex) = configured_account().await;

    // Canonical rows 1..=5; nonce 1 carries a real decodable
    // TransactionSummary, the rest an undecodable stub payload.
    let fixture_payload = load_fixture_delta(1)["delta_payload"].clone();
    seed_delta(&state, &account_id_hex, 1, canonical_at(1), fixture_payload).await;
    for nonce in 2..=5 {
        seed_delta(
            &state,
            &account_id_hex,
            nonce,
            canonical_at(nonce),
            json!({}),
        )
        .await;
    }
    // Non-canonical rows must never appear in history.
    seed_delta(
        &state,
        &account_id_hex,
        6,
        DeltaStatus::Candidate {
            timestamp: "2026-08-01T13:00:00Z".to_string(),
            retry_count: 0,
            divergence_count: 0,
            abandon_requested_at: None,
            abandon_confirm_count: 0,
        },
        json!({}),
    )
    .await;
    seed_delta(
        &state,
        &account_id_hex,
        7,
        DeltaStatus::Discarded {
            timestamp: "2026-08-01T13:01:00Z".to_string(),
            reason: None,
        },
        json!({}),
    )
    .await;

    // Page 1 (limit 2): nonces 5, 4 and a resume cursor.
    let (status, page1) = get_history(&app, &signer, &account_id_hex, Some("2"), None).await;
    assert_eq!(status, StatusCode::OK, "page 1: {page1}");
    let items = page1["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["nonce"], json!(5));
    assert_eq!(items[1]["nonce"], json!(4));
    assert_eq!(items[0]["timestamp"], json!("2026-08-01T12:00:05Z"));
    assert_eq!(items[0]["new_commitment"], json!("0xnew0005"));
    // Undecodable stub payload → empty sections plus a warning, not a 500.
    assert_eq!(items[0]["input_notes"], json!([]));
    assert_eq!(items[0]["output_notes"], json!([]));
    assert_eq!(
        items[0]["decode_warnings"][0]["section"],
        json!("tx_summary")
    );
    let cursor1 = page1["next_cursor"].as_str().expect("page 1 cursor");

    // Page 2: nonces 3, 2.
    let (status, page2) =
        get_history(&app, &signer, &account_id_hex, Some("2"), Some(cursor1)).await;
    assert_eq!(status, StatusCode::OK, "page 2: {page2}");
    let items = page2["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["nonce"], json!(3));
    assert_eq!(items[1]["nonce"], json!(2));
    let cursor2 = page2["next_cursor"].as_str().expect("page 2 cursor");

    // Page 3: nonce 1 only (candidate 6 / discarded 7 filtered out),
    // decoded from the real fixture summary with no warnings.
    let (status, page3) =
        get_history(&app, &signer, &account_id_hex, Some("2"), Some(cursor2)).await;
    assert_eq!(status, StatusCode::OK, "page 3: {page3}");
    let items = page3["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["nonce"], json!(1));
    assert!(
        items[0].get("decode_warnings").is_none(),
        "fixture summary must decode cleanly: {}",
        items[0]
    );
    assert!(items[0]["input_notes"].is_array());
    assert!(items[0]["output_notes"].is_array());
    assert_eq!(page3["next_cursor"], json!(null));
}

#[tokio::test]
async fn test_history_default_limit_and_empty_account() {
    let (_state, app, signer, account_id_hex) = configured_account().await;

    // No deltas at all: empty page, no cursor.
    let (status, body) = get_history(&app, &signer, &account_id_hex, None, None).await;
    assert_eq!(status, StatusCode::OK, "empty history: {body}");
    assert_eq!(body["items"], json!([]));
    assert_eq!(body["next_cursor"], json!(null));
}

#[tokio::test]
async fn test_history_rejects_invalid_limit_and_cursor() {
    let (_state, app, signer, account_id_hex) = configured_account().await;

    let (status, body) = get_history(&app, &signer, &account_id_hex, Some("0"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], json!("invalid_limit"), "{body}");

    let (status, body) = get_history(&app, &signer, &account_id_hex, Some("501"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], json!("invalid_limit"), "{body}");

    let (status, body) =
        get_history(&app, &signer, &account_id_hex, None, Some("not-a-cursor")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], json!("invalid_cursor"), "{body}");
}

#[tokio::test]
async fn test_history_requires_valid_cosigner_auth() {
    let (_state, app, _signer, account_id_hex) = configured_account().await;

    // A signer whose commitment is not in the account's auth set.
    let intruder = TestSigner::new();
    let (status, body) = get_history(&app, &intruder, &account_id_hex, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn test_history_unknown_account_is_404() {
    let (_state, app, signer, account_id_hex) = configured_account().await;

    // Valid account-id format, but never configured: flip the last hex
    // nibble of the fixture id.
    let mut unknown = account_id_hex.clone();
    let last = unknown.pop().expect("non-empty account id");
    unknown.push(if last == '0' { '1' } else { '0' });

    let (status, body) = get_history(&app, &signer, &unknown, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

/// Acceptance criterion (#413): pause state must not block reads.
#[tokio::test]
async fn test_history_served_while_account_paused() {
    let (state, app, signer, account_id_hex) = configured_account().await;
    seed_delta(&state, &account_id_hex, 1, canonical_at(1), json!({})).await;

    state
        .metadata
        .set_pause(&account_id_hex, state.clock.now(), "maintenance")
        .await
        .expect("pause account");

    let (status, body) = get_history(&app, &signer, &account_id_hex, None, None).await;
    assert_eq!(status, StatusCode::OK, "paused reads must work: {body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
}
