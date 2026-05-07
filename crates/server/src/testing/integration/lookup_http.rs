//! Integration tests for `GET /state/lookup`.
//!
//! Covers the live route end-to-end against the test harness in `helpers.rs`,
//! including the load-bearing security properties: 32-byte commitment-as-pubkey
//! rejection, pubkey-commitment binding, and cross-domain replay isolation.

use crate::api::http::LookupResponse;
use crate::testing::helpers::{TestEcdsaSigner, TestSigner, create_router, create_test_app_state};
use crate::testing::integration::lookup_helpers::{
    ecdsa_account, evm_account, falcon_account, fresh_account_id_hex, now_ms, seed, sign_lookup,
    sign_lookup_ecdsa,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::hex::FromHex;
use miden_protocol::Word;
use serde_json::Value;
use tower::Service;

fn lookup_url(key_commitment_hex: &str) -> String {
    format!("/state/lookup?key_commitment={key_commitment_hex}")
}

fn build_lookup_request(
    key_commitment_hex: &str,
    pubkey_hex: &str,
    signature_hex: &str,
    timestamp_ms: i64,
) -> Request<Body> {
    Request::builder()
        .uri(lookup_url(key_commitment_hex))
        .method("GET")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", pubkey_hex)
        .header("x-signature", signature_hex)
        .header("x-timestamp", timestamp_ms.to_string())
        .body(Body::empty())
        .expect("request builder")
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let mut app_clone = app.clone();
    let response = app_clone.call(request).await.expect("router call succeeds");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collect body");
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()))
    };
    (status, body)
}

fn parse_lookup_response(body: &Value) -> LookupResponse {
    serde_json::from_value(body.clone()).expect("LookupResponse parses")
}

// --- Happy path ----------------------------------------------------------

#[tokio::test]
async fn lookup_falcon_happy_path() {
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let account_id = fresh_account_id_hex(1);
    seed(
        &state,
        falcon_account(&account_id, vec![signer.commitment_hex.clone()]),
    )
    .await;
    let app = create_router(state);

    let timestamp = now_ms();
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);
    let request = build_lookup_request(
        &signer.commitment_hex,
        &signer.pubkey_hex,
        &signature,
        timestamp,
    );

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let parsed = parse_lookup_response(&body);
    assert_eq!(parsed.accounts.len(), 1, "exactly one match");
    assert_eq!(parsed.accounts[0].account_id, account_id);
}

#[tokio::test]
async fn lookup_ecdsa_happy_path() {
    // ECDSA-side parity: an account whose authorization set uses
    // Auth::MidenEcdsa must be discoverable via lookup just like a Falcon
    // account. The auth helper resolves the scheme automatically from the
    // pubkey encoding length, so the wire-level surface is identical.
    let state = create_test_app_state().await;
    let signer = TestEcdsaSigner::new();
    let account_id = fresh_account_id_hex(20);
    seed(
        &state,
        ecdsa_account(&account_id, vec![signer.commitment_hex.clone()]),
    )
    .await;
    let app = create_router(state);

    let timestamp = now_ms();
    let signature = sign_lookup_ecdsa(&signer, &signer.commitment_hex, timestamp);
    let request = build_lookup_request(
        &signer.commitment_hex,
        &signer.pubkey_hex,
        &signature,
        timestamp,
    );

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let parsed = parse_lookup_response(&body);
    assert_eq!(parsed.accounts.len(), 1, "exactly one match");
    assert_eq!(parsed.accounts[0].account_id, account_id);
}

#[tokio::test]
async fn lookup_returns_empty_list_for_unauthorized_commitment() {
    // Signer holds a real Falcon key and successfully proves possession, but
    // no account in the metadata store authorizes its commitment. The endpoint
    // must return 200 with `accounts: []`, not 404 — distinguishing "not found"
    // from "wrong key" would leak account presence to non-key-holders.
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    // Deliberately do NOT seed any account for this signer's commitment.
    let app = create_router(state);

    let timestamp = now_ms();
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);
    let request = build_lookup_request(
        &signer.commitment_hex,
        &signer.pubkey_hex,
        &signature,
        timestamp,
    );

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let parsed = parse_lookup_response(&body);
    assert!(
        parsed.accounts.is_empty(),
        "expected empty list, got {body}"
    );
}

#[tokio::test]
async fn lookup_returns_all_matches_for_shared_commitment() {
    // Two accounts authorize the same commitment. The endpoint must surface
    // both account IDs in a single uniform list.
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let account_a = fresh_account_id_hex(2);
    let account_b = fresh_account_id_hex(3);
    seed(
        &state,
        falcon_account(&account_a, vec![signer.commitment_hex.clone()]),
    )
    .await;
    seed(
        &state,
        falcon_account(&account_b, vec![signer.commitment_hex.clone()]),
    )
    .await;
    let app = create_router(state);

    let timestamp = now_ms();
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);
    let request = build_lookup_request(
        &signer.commitment_hex,
        &signer.pubkey_hex,
        &signature,
        timestamp,
    );

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let parsed = parse_lookup_response(&body);
    assert_eq!(parsed.accounts.len(), 2, "expected two matches: {body}");
    let mut ids: Vec<&str> = parsed
        .accounts
        .iter()
        .map(|a| a.account_id.as_str())
        .collect();
    ids.sort();
    let mut expected = [account_a.as_str(), account_b.as_str()];
    expected.sort();
    assert_eq!(ids, expected.to_vec());
}

#[tokio::test]
async fn lookup_excludes_evm_accounts() {
    // EVM rows store `signers`, not `cosigner_commitments`, and must never
    // appear in a Miden-key lookup regardless of the queried commitment value.
    let state = create_test_app_state().await;
    let signer = TestSigner::new();

    // Seed an EVM account whose signers list happens to contain the signer's
    // commitment hex (worst case: byte-identical value).
    seed(
        &state,
        evm_account(
            &fresh_account_id_hex(4),
            vec![signer.commitment_hex.clone()],
        ),
    )
    .await;
    // Seed no Miden account — only the EVM row exists.
    let app = create_router(state);

    let timestamp = now_ms();
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);
    let request = build_lookup_request(
        &signer.commitment_hex,
        &signer.pubkey_hex,
        &signature,
        timestamp,
    );

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let parsed = parse_lookup_response(&body);
    assert!(
        parsed.accounts.is_empty(),
        "EVM accounts must not match Miden-key lookups: {body}"
    );
}

// --- Negative paths ------------------------------------------------------

#[tokio::test]
async fn lookup_rejects_missing_key_commitment_query_param() {
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let app = create_router(state);

    let timestamp = now_ms();
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);
    // No `key_commitment` query parameter at all.
    let request = Request::builder()
        .uri("/state/lookup")
        .method("GET")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature)
        .header("x-timestamp", timestamp.to_string())
        .body(Body::empty())
        .unwrap();

    let (status, _body) = send(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn lookup_rejects_malformed_key_commitment_hex() {
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let app = create_router(state);

    let timestamp = now_ms();
    // Anything would do — service rejects the commitment format before
    // verifying the signature.
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);
    let request = build_lookup_request(
        "0xnot-hex-at-all",
        &signer.pubkey_hex,
        &signature,
        timestamp,
    );

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("invalid_input"),
        "body: {body}"
    );
}

#[tokio::test]
async fn lookup_rejects_32_byte_commitment_as_pubkey() {
    // Load-bearing security check: even with a syntactically perfect request
    // body and a successful signature attempt, submitting a 32-byte raw
    // commitment as `x-pubkey` (the alias accepted by
    // Auth::compute_signer_commitment elsewhere) MUST be rejected as
    // invalid_input by the lookup path.
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let app = create_router(state);

    let timestamp = now_ms();
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);

    // 32-byte raw value posing as an x-pubkey.
    let request = build_lookup_request(
        &signer.commitment_hex,
        &signer.commitment_hex,
        &signature,
        timestamp,
    );

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("invalid_input"),
        "body: {body}"
    );
}

#[tokio::test]
async fn lookup_rejects_pubkey_commitment_mismatch() {
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let other_signer = TestSigner::new();
    let app = create_router(state);

    let timestamp = now_ms();
    // Sign over the queried commitment with our REAL key…
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);
    // …but submit a different pubkey whose commitment does NOT match.
    assert_ne!(signer.commitment_hex, other_signer.commitment_hex);
    let request = build_lookup_request(
        &signer.commitment_hex,
        &other_signer.pubkey_hex,
        &signature,
        timestamp,
    );

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("authentication_failed"),
        "body: {body}"
    );
}

#[tokio::test]
async fn lookup_rejects_tampered_signature() {
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let app = create_router(state);

    let timestamp = now_ms();
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);
    // Flip a byte in the middle of the hex signature to corrupt it.
    let mut tampered = signature.into_bytes();
    let mid = tampered.len() / 2;
    // Swap one hex character to a different one (deterministically).
    tampered[mid] = if tampered[mid] == b'0' { b'1' } else { b'0' };
    let tampered_hex = String::from_utf8(tampered).unwrap();

    let request = build_lookup_request(
        &signer.commitment_hex,
        &signer.pubkey_hex,
        &tampered_hex,
        timestamp,
    );

    let (status, _body) = send(&app, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn lookup_rejects_timestamp_outside_skew_window() {
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let app = create_router(state);

    // 10 minutes in the past — outside the 5-minute skew window.
    let stale_timestamp = now_ms() - 10 * 60 * 1000;
    let signature = sign_lookup(&signer, &signer.commitment_hex, stale_timestamp);
    let request = build_lookup_request(
        &signer.commitment_hex,
        &signer.pubkey_hex,
        &signature,
        stale_timestamp,
    );

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("authentication_failed"),
        "body: {body}"
    );
}

#[tokio::test]
async fn lookup_rejects_signature_signed_for_auth_request_message() {
    // Cross-domain replay: a signature crafted for AuthRequestMessage (the
    // per-account request-signing format) MUST NOT validate against the
    // lookup endpoint, even if the timestamp is fresh and the pubkey-
    // commitment binding holds.
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let account_id = fresh_account_id_hex(5);
    seed(
        &state,
        falcon_account(&account_id, vec![signer.commitment_hex.clone()]),
    )
    .await;
    let app = create_router(state);

    let timestamp = now_ms();

    // Build the AuthRequestMessage digest directly (an account-bound digest)
    // and sign it with our real Falcon key. This bypasses TestSigner's
    // helpers, which already build account-bound signatures, but does so
    // explicitly to make the cross-domain intent obvious.
    let key_commitment_word =
        Word::from_hex(&signer.commitment_hex).expect("commitment is valid Word");
    let payload = AuthRequestPayload::from_bytes(&key_commitment_word.as_bytes());
    let request_digest = AuthRequestMessage::from_account_id_hex(&account_id, timestamp, payload)
        .expect("valid account id")
        .to_word();
    let bad_signature = signer.sign_word(request_digest);

    let request = build_lookup_request(
        &signer.commitment_hex,
        &signer.pubkey_hex,
        &bad_signature,
        timestamp,
    );

    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("authentication_failed"),
        "body: {body}"
    );
}
