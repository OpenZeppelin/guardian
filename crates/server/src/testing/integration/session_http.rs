//! HTTP integration tests for Miden account sessions: a wallet signs a
//! session grant once, then a P-256 delegated signer signs per-account
//! requests with `x-auth-format: session`.

use super::lookup_helpers::{lookup_digest, now_ms};
use super::session_helpers::{
    GrantInput, MAX_TTL_SECONDS, logout_signature, revoke_all_eip712_signature,
    revoke_all_signature, revoke_all_word, seed_cosigner, with_max_ttl,
};
use crate::builder::clock::test::MockClock;
use crate::metadata::auth::Auth;
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::session::MidenSession;
use crate::state::AppState;
use crate::testing::helpers::{
    IntegrationMockNetworkClient, TestEcdsaSigner, TestSigner, create_router,
    create_test_app_state, fixture_signer, load_fixture_account, load_fixture_delta,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use guardian_shared::auth_request_eip712::session_digest;
use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::session_key::SessionKey;
use guardian_shared::{FromJson, SignatureScheme};
use miden_protocol::Word;
use miden_protocol::transaction::TransactionSummary;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::Service;

/// The fixture account configured by its first cosigner.
async fn configured_account() -> (axum::Router, AppState, TestSigner, String) {
    configured_account_with(with_max_ttl(create_test_app_state().await)).await
}

/// [`configured_account`] on `state`, e.g. with a frozen clock.
async fn configured_account_with(
    mut state: AppState,
) -> (axum::Router, AppState, TestSigner, String) {
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
                .body(Body::from(serde_json::to_string(&configure_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "configure");

    (app, state, signer, account_id_hex)
}

async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().call(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// Asserts an error response's status and stable code.
fn assert_error(response: &(StatusCode, Value), status: StatusCode, code: &str, context: &str) {
    assert_eq!(response.0, status, "{context}: {}", response.1);
    assert_eq!(response.1["code"], code, "{context}");
}

async fn post_session(app: &axum::Router, body: &Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .uri("/session")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    send(app, request).await
}

/// Registers a session for the Falcon `signer` and returns its key.
async fn falcon_session(app: &axum::Router, state: &AppState, signer: &TestSigner) -> SessionKey {
    let input = GrantInput::for_state(state, &signer.commitment_hex, SignatureScheme::Falcon);
    falcon_session_for(app, &input, signer).await
}

async fn falcon_session_for(
    app: &axum::Router,
    input: &GrantInput,
    signer: &TestSigner,
) -> SessionKey {
    let session_key = SessionKey::generate();
    let signature = signer.sign_word(input.grant(&session_key).to_word());
    let (status, body) = post_session(app, &input.body(&session_key, "falcon", &signature)).await;
    assert_eq!(status, StatusCode::OK, "create session: {body}");
    assert_eq!(body["signer_commitment"], signer.commitment_hex);
    session_key
}

/// A request signed by the session key over `message`.
fn signed_by_session(
    method: &str,
    uri: &str,
    session_key: &SessionKey,
    message: Word,
    timestamp: i64,
    body: Body,
) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .method(method)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", session_key.public_key_hex())
        .header("x-signature", session_key.sign_hex(message))
        .header("x-timestamp", timestamp.to_string())
        .header("x-auth-format", "session")
        .body(body)
        .unwrap()
}

fn request_word(account_id_hex: &str, timestamp: i64, payload: &Value) -> Word {
    AuthRequestMessage::from_account_id_hex(
        account_id_hex,
        timestamp,
        AuthRequestPayload::from_json_serializable(payload).unwrap(),
    )
    .unwrap()
    .to_word()
}

/// `GET {path}?account_id=...` signed by the session key at `timestamp`.
async fn session_get_with(
    app: &axum::Router,
    session_key: &SessionKey,
    path: &str,
    account_id_hex: &str,
    timestamp: i64,
) -> (StatusCode, Value) {
    let message = request_word(
        account_id_hex,
        timestamp,
        &json!({ "account_id": account_id_hex }),
    );
    send(
        app,
        signed_by_session(
            "GET",
            &format!("{path}?account_id={account_id_hex}"),
            session_key,
            message,
            timestamp,
            Body::empty(),
        ),
    )
    .await
}

async fn session_get(
    app: &axum::Router,
    session_key: &SessionKey,
    account_id_hex: &str,
) -> (StatusCode, Value) {
    session_get_with(app, session_key, "/state", account_id_hex, now_ms()).await
}

/// `{method} {path}` with a JSON body signed by the session key as an account request.
async fn session_send_json(
    app: &axum::Router,
    session_key: &SessionKey,
    method: &str,
    path: &str,
    account_id_hex: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let timestamp = now_ms();
    send(
        app,
        signed_by_session(
            method,
            path,
            session_key,
            request_word(account_id_hex, timestamp, body),
            timestamp,
            Body::from(body.to_string()),
        ),
    )
    .await
}

/// Wallet-signed `GET /state` at `timestamp`.
async fn wallet_get_state(
    app: &axum::Router,
    signer: &TestSigner,
    account_id_hex: &str,
    timestamp: i64,
) -> (StatusCode, Value) {
    let payload =
        AuthRequestPayload::from_json_serializable(&json!({ "account_id": account_id_hex }))
            .unwrap();
    let (signature, _) =
        signer.sign_with_timestamp_and_request(account_id_hex, timestamp, &payload);
    send(
        app,
        Request::builder()
            .uri(format!("/state?account_id={account_id_hex}"))
            .header("x-pubkey", &signer.pubkey_hex)
            .header("x-signature", signature)
            .header("x-timestamp", timestamp.to_string())
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn logout(app: &axum::Router, session_key: &SessionKey) -> (StatusCode, Value) {
    let (signature, timestamp) = logout_signature(session_key);
    send(
        app,
        Request::builder()
            .uri("/session/logout")
            .method("POST")
            .header("x-pubkey", session_key.public_key_hex())
            .header("x-signature", signature)
            .header("x-timestamp", timestamp.to_string())
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// `POST /session/revoke-all` with the given wallet signature.
fn revoke_all_request(
    signer_commitment: &str,
    pubkey_hex: &str,
    signature: &str,
    timestamp: i64,
    auth_format: Option<&str>,
) -> Request<Body> {
    let mut request = Request::builder()
        .uri("/session/revoke-all")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", pubkey_hex)
        .header("x-signature", signature)
        .header("x-timestamp", timestamp.to_string());
    if let Some(format) = auth_format {
        request = request.header("x-auth-format", format);
    }
    request
        .body(Body::from(
            json!({ "signer_commitment": signer_commitment }).to_string(),
        ))
        .unwrap()
}

/// Wallet-signed `POST /session/revoke-all` for a Falcon signer.
async fn revoke_all(app: &axum::Router, signer: &TestSigner) -> (StatusCode, Value) {
    let (signature, timestamp) = revoke_all_signature(signer);
    send(
        app,
        revoke_all_request(
            &signer.commitment_hex,
            &signer.pubkey_hex,
            &signature,
            timestamp,
            None,
        ),
    )
    .await
}

/// The fixture proposal as a `POST /delta/proposal` body.
fn proposal_body(account_id_hex: &str, signer: &TestSigner) -> Value {
    json!({
        "account_id": account_id_hex,
        "nonce": 1,
        "delta_payload": {
            "tx_summary": load_fixture_delta(1)["delta_payload"],
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [signer.commitment_hex.clone()]
            }
        }
    })
}

#[tokio::test]
async fn test_status_advertises_sessions() {
    let app = create_router(with_max_ttl(create_test_app_state().await));
    let status_request = || {
        Request::builder()
            .uri("/status")
            .body(Body::empty())
            .unwrap()
    };
    let (status, body) = send(&app, status_request()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sessions"]["max_ttl_seconds"], MAX_TTL_SECONDS);

    // Sessions are always on; the default maximum lifetime is 8 hours.
    let default_app = create_router(create_test_app_state().await);
    let (_, body) = send(&default_app, status_request()).await;
    assert_eq!(body["sessions"]["max_ttl_seconds"], 28_800);
}

#[tokio::test]
async fn test_session_lifecycle() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let session_key = falcon_session(&app, &state, &signer).await;

    // Explicit, increasing timestamps: both reads advance one floor.
    let start = now_ms();
    for (offset, path) in ["/state", "/state/nonce"].into_iter().enumerate() {
        let (status, body) = session_get_with(
            &app,
            &session_key,
            path,
            &account_id_hex,
            start + offset as i64,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }

    let (status, body) = logout(&app, &session_key).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["revoked"], true);

    let response = session_get(&app, &session_key, &account_id_hex).await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "session_revoked",
        "after logout",
    );

    let (status, body) = logout(&app, &session_key).await;
    assert_eq!(status, StatusCode::OK, "logout is idempotent");
    assert_eq!(body["revoked"], false);
}

#[tokio::test]
async fn test_logout_of_an_unknown_session_is_idempotent() {
    let app = create_router(with_max_ttl(create_test_app_state().await));
    let (status, body) = logout(&app, &SessionKey::generate()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revoked"], false);

    // Only once the delegated signature verifies.
    let key = SessionKey::generate();
    let (_, timestamp) = logout_signature(&key);
    let response = send(
        &app,
        Request::builder()
            .uri("/session/logout")
            .method("POST")
            .header("x-pubkey", key.public_key_hex())
            .header("x-signature", key.sign_hex(Word::default()))
            .header("x-timestamp", timestamp.to_string())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "authentication_failed",
        "forged logout",
    );
}

#[tokio::test]
async fn test_expired_session_reports_session_expired() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let session_key = SessionKey::generate();
    let now = state.clock.now();
    let secs = now.timestamp() as u64;
    state
        .miden_sessions
        .register(
            &session_key.public_key(),
            MidenSession {
                signer_commitment: signer.commitment_hex.clone(),
                origin: String::new(),
                guardian_commitment: state.ack.commitment(&SignatureScheme::Falcon),
                network: state.dashboard.environment().to_string(),
            },
            secs - 600,
            secs - 1,
            now,
        )
        .await
        .unwrap();

    let response = session_get(&app, &session_key, &account_id_hex).await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "session_expired",
        "expired",
    );
}

#[tokio::test]
async fn test_session_signature_is_bound_to_the_request() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let session_key = falcon_session(&app, &state, &signer).await;

    // Signed for another account, sent for this one.
    let timestamp = now_ms();
    let other_account = "0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c";
    let message = request_word(
        other_account,
        timestamp,
        &json!({ "account_id": other_account }),
    );
    let response = send(
        &app,
        signed_by_session(
            "GET",
            &format!("/state?account_id={account_id_hex}"),
            &session_key,
            message,
            timestamp,
            Body::empty(),
        ),
    )
    .await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "authentication_failed",
        "other account",
    );
}

#[tokio::test]
async fn test_unregistered_session_key_is_rejected() {
    let (app, _state, _signer, account_id_hex) = configured_account().await;
    let response = session_get(&app, &SessionKey::generate(), &account_id_hex).await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "authentication_failed",
        "unknown key",
    );
}

#[tokio::test]
async fn test_grant_signer_must_cosign_an_account() {
    let (app, state, _signer, account_id_hex) = configured_account().await;

    // A key that cosigns nothing cannot register a session at all.
    let outsider = TestSigner::new();
    let session_key = SessionKey::generate();
    let input = GrantInput::for_state(&state, &outsider.commitment_hex, SignatureScheme::Falcon);
    let signature = outsider.sign_word(input.grant(&session_key).to_word());
    let response = post_session(&app, &input.body(&session_key, "falcon", &signature)).await;
    assert_error(
        &response,
        StatusCode::FORBIDDEN,
        "authorization_failed",
        "outsider",
    );

    // A cosigner of another account registers, but cannot use this one.
    seed_cosigner(&state, &outsider.commitment_hex, SignatureScheme::Falcon).await;
    let session_key = falcon_session(&app, &state, &outsider).await;
    let response = session_get(&app, &session_key, &account_id_hex).await;
    assert_error(
        &response,
        StatusCode::FORBIDDEN,
        "authorization_failed",
        "another account's cosigner",
    );
}

#[tokio::test]
async fn test_removed_signer_loses_access_until_added_back() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let session_key = falcon_session(&app, &state, &signer).await;
    let start = now_ms();
    let (status, _) = session_get_with(&app, &session_key, "/state", &account_id_hex, start).await;
    assert_eq!(status, StatusCode::OK);

    let mut metadata = state.metadata.get(&account_id_hex).await.unwrap().unwrap();
    let original_auth = metadata.auth.clone();
    metadata.auth = Auth::MidenFalconRpo {
        cosigner_commitments: vec![TestSigner::new().commitment_hex],
    };
    state.metadata.set(metadata.clone()).await.unwrap();
    let response = session_get_with(&app, &session_key, "/state", &account_id_hex, start + 1).await;
    assert_error(
        &response,
        StatusCode::FORBIDDEN,
        "authorization_failed",
        "removed signer",
    );

    // Adding the signer back restores the unexpired grant.
    metadata.auth = original_auth;
    state.metadata.set(metadata).await.unwrap();
    let (status, body) =
        session_get_with(&app, &session_key, "/state", &account_id_hex, start + 2).await;
    assert_eq!(status, StatusCode::OK, "re-added signer: {body}");
}

#[tokio::test]
async fn test_proposals_are_created_and_signed_through_a_session() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let session_key = falcon_session(&app, &state, &signer).await;

    let body = proposal_body(&account_id_hex, &signer);
    let (status, created) = session_send_json(
        &app,
        &session_key,
        "POST",
        "/delta/proposal",
        &account_id_hex,
        &body,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create through the session: {created}"
    );
    let commitment = created["commitment"].as_str().unwrap().to_string();
    let summary = TransactionSummary::from_json(&load_fixture_delta(1)["delta_payload"]).unwrap();
    let sign_body = |signature: String| {
        json!({
            "account_id": account_id_hex,
            "commitment": commitment,
            "signature": { "scheme": "falcon", "signature": signature }
        })
    };

    // The delegated signer cannot approve: approvals are verified against
    // the transaction summary and must come from the wallet key (FR-010).
    let forged = sign_body(signer.sign_word(Word::default()));
    let response = session_send_json(
        &app,
        &session_key,
        "PUT",
        "/delta/proposal",
        &account_id_hex,
        &forged,
    )
    .await;
    assert_error(
        &response,
        StatusCode::BAD_REQUEST,
        "invalid_proposal_signature",
        "approval not over the summary",
    );

    // The wallet's approval, carried by a session-signed request.
    let approval = sign_body(signer.sign_word(summary.to_commitment()));
    let (status, body) = session_send_json(
        &app,
        &session_key,
        "PUT",
        "/delta/proposal",
        &account_id_hex,
        &approval,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "wallet approval: {body}");
}

#[tokio::test]
async fn test_wallet_only_routes_reject_the_session() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let session_key = falcon_session(&app, &state, &signer).await;
    let (_account_id, _, initial_state) = load_fixture_account();

    // Account routes: the delegated signature verifies, then the route refuses it.
    let posts = [
        ("/delta", load_fixture_delta(1)),
        (
            "/delta/candidate/abandon",
            json!({ "account_id": account_id_hex, "nonce": 1 }),
        ),
        (
            "/configure",
            json!({
                "account_id": account_id_hex,
                "auth": { "MidenFalconRpo": { "cosigner_commitments": [signer.commitment_hex] } },
                "initial_state": initial_state
            }),
        ),
    ];
    for (path, body) in &posts {
        let response =
            session_send_json(&app, &session_key, "POST", path, &account_id_hex, body).await;
        assert_error(
            &response,
            StatusCode::FORBIDDEN,
            "wallet_signature_required",
            path,
        );
    }

    // Account-less routes: the session signs the route's own message.
    let timestamp = now_ms();
    let lookup_uri = format!("/state/lookup?key_commitment={}", signer.commitment_hex);
    let response = send(
        &app,
        signed_by_session(
            "GET",
            &lookup_uri,
            &session_key,
            lookup_digest(&signer.commitment_hex, timestamp),
            timestamp,
            Body::empty(),
        ),
    )
    .await;
    assert_error(
        &response,
        StatusCode::FORBIDDEN,
        "wallet_signature_required",
        "lookup",
    );

    let response = send(
        &app,
        signed_by_session(
            "POST",
            "/session/revoke-all",
            &session_key,
            revoke_all_word(&signer.commitment_hex, timestamp),
            timestamp,
            Body::from(json!({ "signer_commitment": signer.commitment_hex }).to_string()),
        ),
    )
    .await;
    assert_error(
        &response,
        StatusCode::FORBIDDEN,
        "wallet_signature_required",
        "revoke-all",
    );

    // Nothing changed: the session still works, and the wallet still can.
    let (status, _) = session_get(&app, &session_key, &account_id_hex).await;
    assert_eq!(status, StatusCode::OK);
    let timestamp = now_ms();
    let (status, body) = send(
        &app,
        Request::builder()
            .uri(&lookup_uri)
            .header("x-pubkey", &signer.pubkey_hex)
            .header(
                "x-signature",
                signer.sign_word(lookup_digest(&signer.commitment_hex, timestamp)),
            )
            .header("x-timestamp", timestamp.to_string())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "wallet lookup: {body}");
    let (status, body) = revoke_all(&app, &signer).await;
    assert_eq!(status, StatusCode::OK, "wallet revoke-all: {body}");
    assert_eq!(body["revoked"], 1);
}

/// FR-007 order: the route allow-list comes right after the session resolves,
/// before the grant's Guardian key, network and cosigner checks, and an ended
/// session is reported as ended.
#[tokio::test]
async fn test_wallet_only_routes_answer_before_the_grant_checks() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let now = state.clock.now();
    let secs = now.timestamp() as u64;
    let outsider = TestSigner::new();
    seed_cosigner(&state, &outsider.commitment_hex, SignatureScheme::Falcon).await;
    let not_a_cosigner = falcon_session(&app, &state, &outsider).await;
    let rotated_key = SessionKey::generate();
    state
        .miden_sessions
        .register(
            &rotated_key.public_key(),
            MidenSession {
                signer_commitment: signer.commitment_hex.clone(),
                origin: String::new(),
                guardian_commitment: format!("0x{}", "11".repeat(32)),
                network: state.dashboard.environment().to_string(),
            },
            secs,
            secs + 600,
            now,
        )
        .await
        .unwrap();
    let posts = [
        ("/delta", load_fixture_delta(1)),
        (
            "/delta/candidate/abandon",
            json!({ "account_id": account_id_hex, "nonce": 1 }),
        ),
    ];
    for (name, session_key) in [
        ("signer does not cosign the account", &not_a_cosigner),
        ("grant names a rotated Guardian key", &rotated_key),
    ] {
        for (path, body) in &posts {
            let response =
                session_send_json(&app, session_key, "POST", path, &account_id_hex, body).await;
            assert_error(
                &response,
                StatusCode::FORBIDDEN,
                "wallet_signature_required",
                &format!("{path}: {name}"),
            );
        }
    }

    let ended = falcon_session(&app, &state, &signer).await;
    let (status, body) = logout(&app, &ended).await;
    assert_eq!(status, StatusCode::OK, "logout: {body}");
    let response =
        session_send_json(&app, &ended, "POST", "/delta", &account_id_hex, &posts[0].1).await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "session_revoked",
        "ended session on a wallet-only route",
    );
}

/// Every FR-008 read accepts the session: none of them answers with an
/// authentication or authorization error.
#[tokio::test]
async fn test_every_session_eligible_read_accepts_the_session() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let session_key = falcon_session(&app, &state, &signer).await;
    let commitment = format!("0x{}", "ab".repeat(32));
    let reads = [
        ("/state", json!({ "account_id": account_id_hex })),
        ("/state/nonce", json!({ "account_id": account_id_hex })),
        (
            "/delta",
            json!({ "account_id": account_id_hex, "nonce": 1 }),
        ),
        (
            "/delta/since",
            json!({ "account_id": account_id_hex, "nonce": 1 }),
        ),
        ("/delta/history", json!({ "account_id": account_id_hex })),
        ("/delta/proposal", json!({ "account_id": account_id_hex })),
        (
            "/delta/proposal/single",
            json!({ "account_id": account_id_hex, "commitment": commitment }),
        ),
    ];
    let start = now_ms();
    for (offset, (path, query)) in (0..).zip(&reads) {
        let timestamp = start + offset;
        let query_string = query
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| match value {
                Value::String(text) => format!("{key}={text}"),
                other => format!("{key}={other}"),
            })
            .collect::<Vec<_>>()
            .join("&");
        let (status, body) = send(
            &app,
            signed_by_session(
                "GET",
                &format!("{path}?{query_string}"),
                &session_key,
                request_word(&account_id_hex, timestamp, query),
                timestamp,
                Body::empty(),
            ),
        )
        .await;
        assert!(
            status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
            "{path} refused the session: {status} {body}"
        );
    }
}

#[tokio::test]
async fn test_forged_session_credentials_on_wallet_only_routes_fail_authentication() {
    let (app, _state, signer, account_id_hex) = configured_account().await;
    let forged = SessionKey::generate();
    let (_account_id, _, initial_state) = load_fixture_account();
    let posts = [
        ("/delta", load_fixture_delta(1)),
        (
            "/delta/candidate/abandon",
            json!({ "account_id": account_id_hex, "nonce": 1 }),
        ),
        (
            "/configure",
            json!({
                "account_id": account_id_hex,
                "auth": { "MidenFalconRpo": { "cosigner_commitments": [signer.commitment_hex] } },
                "initial_state": initial_state
            }),
        ),
    ];
    // Unauthenticated callers learn nothing about route policy.
    for (path, body) in &posts {
        let response = session_send_json(&app, &forged, "POST", path, &account_id_hex, body).await;
        assert_error(
            &response,
            StatusCode::UNAUTHORIZED,
            "authentication_failed",
            path,
        );
    }
    let timestamp = now_ms();
    let response = send(
        &app,
        signed_by_session(
            "GET",
            &format!("/state/lookup?key_commitment={}", signer.commitment_hex),
            &forged,
            lookup_digest(&signer.commitment_hex, timestamp),
            timestamp,
            Body::empty(),
        ),
    )
    .await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "authentication_failed",
        "lookup",
    );
}

#[tokio::test]
async fn test_ecdsa_raw_and_eip712_grants() {
    let state = with_max_ttl(create_test_app_state().await);
    let app = create_router(state.clone());
    let signer = TestEcdsaSigner::new();
    seed_cosigner(&state, &signer.commitment_hex, SignatureScheme::Ecdsa).await;
    let input = GrantInput::for_state(&state, &signer.commitment_hex, SignatureScheme::Ecdsa);
    let body =
        |key: &SessionKey, signature: String, public_key: bool, auth_format: Option<&str>| {
            let mut body = input.body(key, "ecdsa", &signature);
            if public_key {
                body["public_key"] = json!(signer.pubkey_hex);
            }
            if let Some(format) = auth_format {
                body["auth_format"] = json!(format);
            }
            body
        };

    let raw = SessionKey::generate();
    let signature = signer.sign_word(input.grant(&raw).to_word());
    let (status, response) = post_session(&app, &body(&raw, signature, true, None)).await;
    assert_eq!(status, StatusCode::OK, "raw: {response}");

    // Raw ECDSA recovers the key, so the public key is optional.
    let recovered = SessionKey::generate();
    let signature = signer.sign_word(input.grant(&recovered).to_word());
    let (status, response) = post_session(&app, &body(&recovered, signature, false, None)).await;
    assert_eq!(status, StatusCode::OK, "raw without public key: {response}");

    let typed = SessionKey::generate();
    let signature = signer.sign_prehash(session_digest(&input.grant(&typed)));
    let (status, response) =
        post_session(&app, &body(&typed, signature, true, Some("eip712"))).await;
    assert_eq!(status, StatusCode::OK, "eip712: {response}");

    // EIP-712 always reads the supplied key, and a raw signature is not typed data.
    let missing_key = SessionKey::generate();
    let signature = signer.sign_prehash(session_digest(&input.grant(&missing_key)));
    let response = post_session(&app, &body(&missing_key, signature, false, Some("eip712"))).await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "authentication_failed",
        "eip712 without a key",
    );
    let mixed = SessionKey::generate();
    let signature = signer.sign_word(input.grant(&mixed).to_word());
    let response = post_session(&app, &body(&mixed, signature, true, Some("eip712"))).await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "authentication_failed",
        "raw signature as eip712",
    );

    // The wallet revokes its three sessions with EIP-712 typed data.
    let (signature, timestamp) = revoke_all_eip712_signature(&signer);
    let (status, response) = send(
        &app,
        revoke_all_request(
            &signer.commitment_hex,
            &signer.pubkey_hex,
            &signature,
            timestamp,
            Some("eip712"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "eip712 revoke-all: {response}");
    assert_eq!(response["revoked"], 3);
}

#[tokio::test]
async fn test_raw_ecdsa_grant_and_revoke_all_fall_back_to_the_supplied_key() {
    let state = with_max_ttl(create_test_app_state().await);
    let app = create_router(state.clone());
    let signer = TestEcdsaSigner::new();
    seed_cosigner(&state, &signer.commitment_hex, SignatureScheme::Ecdsa).await;
    // A wallet whose recovery byte does not recover its own key.
    let flip_recovery = |signature: String| {
        let mut bytes = hex::decode(signature.trim_start_matches("0x")).unwrap();
        bytes[64] ^= 1;
        format!("0x{}", hex::encode(bytes))
    };

    let input = GrantInput::for_state(&state, &signer.commitment_hex, SignatureScheme::Ecdsa);
    let key = SessionKey::generate();
    let mut body = input.body(
        &key,
        "ecdsa",
        &flip_recovery(signer.sign_word(input.grant(&key).to_word())),
    );
    body["public_key"] = json!(signer.pubkey_hex);
    let (status, response) = post_session(&app, &body).await;
    assert_eq!(status, StatusCode::OK, "grant: {response}");

    let timestamp = now_ms();
    let signature =
        flip_recovery(signer.sign_word(revoke_all_word(&signer.commitment_hex, timestamp)));
    let (status, response) = send(
        &app,
        revoke_all_request(
            &signer.commitment_hex,
            &signer.pubkey_hex,
            &signature,
            timestamp,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "revoke-all: {response}");
    assert_eq!(response["revoked"], 1);
}

#[tokio::test]
async fn test_create_session_rejects_grants_that_do_not_match_this_guardian() {
    let state = with_max_ttl(create_test_app_state().await);
    let app = create_router(state.clone());
    let signer = TestSigner::new();
    seed_cosigner(&state, &signer.commitment_hex, SignatureScheme::Falcon).await;
    let base = || GrantInput::for_state(&state, &signer.commitment_hex, SignatureScheme::Falcon);

    let cases: Vec<(&str, GrantInput, StatusCode, &str)> = vec![
        (
            "other network",
            GrantInput {
                network: "nowhere".to_string(),
                ..base()
            },
            StatusCode::BAD_REQUEST,
            "invalid_input",
        ),
        (
            "other guardian key",
            GrantInput {
                guardian_commitment: format!("0x{}", "11".repeat(32)),
                ..base()
            },
            StatusCode::BAD_REQUEST,
            "invalid_input",
        ),
        (
            "ttl above the maximum",
            GrantInput {
                expires_at: base().issued_at + u64::from(MAX_TTL_SECONDS) + 60,
                ..base()
            },
            StatusCode::BAD_REQUEST,
            "invalid_input",
        ),
        (
            "already expired",
            GrantInput {
                issued_at: base().issued_at - 120,
                expires_at: base().issued_at - 60,
                ..base()
            },
            StatusCode::BAD_REQUEST,
            "invalid_input",
        ),
        (
            "expires inside the clock-skew window",
            GrantInput {
                expires_at: base().issued_at + 200,
                ..base()
            },
            StatusCode::BAD_REQUEST,
            "invalid_input",
        ),
        (
            "stale issued_at",
            GrantInput {
                issued_at: base().issued_at - 3_000,
                ..base()
            },
            StatusCode::UNAUTHORIZED,
            "authentication_failed",
        ),
        (
            "signer commitment of another key",
            GrantInput {
                signer_commitment: TestSigner::new().commitment_hex,
                ..base()
            },
            StatusCode::UNAUTHORIZED,
            "authentication_failed",
        ),
    ];

    for (name, input, status, code) in cases {
        let session_key = SessionKey::generate();
        // Sign whatever is presented, so only the server-side check rejects it.
        let signature = signer.sign_word(input.grant(&session_key).to_word());
        let response = post_session(&app, &input.body(&session_key, "falcon", &signature)).await;
        assert_error(&response, status, code, name);
    }

    // A signature by a different wallet.
    let session_key = SessionKey::generate();
    let input = base();
    let forged = TestSigner::new().sign_word(input.grant(&session_key).to_word());
    let response = post_session(&app, &input.body(&session_key, "falcon", &forged)).await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "authentication_failed",
        "another wallet's signature",
    );
}

#[tokio::test]
async fn test_resubmitted_grant_is_idempotent_until_revoked() {
    let state = with_max_ttl(create_test_app_state().await);
    let app = create_router(state.clone());
    let signer = TestSigner::new();
    seed_cosigner(&state, &signer.commitment_hex, SignatureScheme::Falcon).await;
    let input = GrantInput::for_state(&state, &signer.commitment_hex, SignatureScheme::Falcon);

    let session_key = SessionKey::generate();
    let signature = signer.sign_word(input.grant(&session_key).to_word());
    let body = input.body(&session_key, "falcon", &signature);
    let (status, first) = post_session(&app, &body).await;
    assert_eq!(status, StatusCode::OK);
    let (status, again) = post_session(&app, &body).await;
    assert_eq!(status, StatusCode::OK, "re-submit: {again}");
    assert_eq!(again["expires_at"], first["expires_at"]);

    // A different grant for the same key is refused.
    let longer = GrantInput {
        expires_at: input.expires_at + 1,
        ..GrantInput::for_state(&state, &signer.commitment_hex, SignatureScheme::Falcon)
    };
    let signature = signer.sign_word(longer.grant(&session_key).to_word());
    let response = post_session(&app, &longer.body(&session_key, "falcon", &signature)).await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "authentication_failed",
        "same key, different grant",
    );

    // The grant of a revoked session stays dead.
    assert_eq!(logout(&app, &session_key).await.0, StatusCode::OK);
    let response = post_session(&app, &body).await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "authentication_failed",
        "revoked",
    );
}

/// FR-006: a grant the wallet re-signed with a later `issued_at` is the same
/// session and keeps the recorded issue time, so a revoke-all signed between
/// the two still ends it.
#[tokio::test]
async fn test_a_re_signed_grant_cannot_escape_revoke_all() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let now = state.clock.now().timestamp() as u64;
    let first = GrantInput {
        issued_at: now - 60,
        ..GrantInput::for_state(&state, &signer.commitment_hex, SignatureScheme::Falcon)
    };
    let session_key = SessionKey::generate();
    for input in [
        &first,
        &GrantInput {
            issued_at: now,
            ..first.clone()
        },
    ] {
        let signature = signer.sign_word(input.grant(&session_key).to_word());
        let (status, body) =
            post_session(&app, &input.body(&session_key, "falcon", &signature)).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "issued_at {}: {body}",
            input.issued_at
        );
    }

    let between = ((now - 30) * 1000) as i64;
    let signature = signer.sign_word(revoke_all_word(&signer.commitment_hex, between));
    let (status, body) = send(
        &app,
        revoke_all_request(
            &signer.commitment_hex,
            &signer.pubkey_hex,
            &signature,
            between,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revoked"], 1);
    let response = session_get(&app, &session_key, &account_id_hex).await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "session_revoked",
        "re-signed grant",
    );
}

/// SC-003: an origin over 256 bytes is refused before any signature check.
#[tokio::test]
async fn test_create_session_rejects_an_origin_over_256_bytes() {
    let state = with_max_ttl(create_test_app_state().await);
    let app = create_router(state.clone());
    let input = GrantInput {
        origin: format!("https://{}", "a".repeat(250)),
        ..GrantInput::for_state(
            &state,
            &TestSigner::new().commitment_hex,
            SignatureScheme::Falcon,
        )
    };
    let response = post_session(&app, &input.body(&SessionKey::generate(), "falcon", "0x00")).await;
    assert_error(
        &response,
        StatusCode::BAD_REQUEST,
        "invalid_input",
        "long origin",
    );
}

/// A grant for this Guardian's key of the other scheme cannot reach a Falcon
/// account, but the session stays valid for the signer's own accounts.
#[tokio::test]
async fn test_a_grant_for_the_other_scheme_keeps_the_session() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let now = state.clock.now();
    let secs = now.timestamp() as u64;
    let session_key = SessionKey::generate();
    state
        .miden_sessions
        .register(
            &session_key.public_key(),
            MidenSession {
                signer_commitment: signer.commitment_hex.clone(),
                origin: String::new(),
                guardian_commitment: state.ack.commitment(&SignatureScheme::Ecdsa),
                network: state.dashboard.environment().to_string(),
            },
            secs,
            secs + 600,
            now,
        )
        .await
        .unwrap();

    let response = session_get(&app, &session_key, &account_id_hex).await;
    assert_error(
        &response,
        StatusCode::FORBIDDEN,
        "authorization_failed",
        "grant for the other scheme",
    );
}

#[tokio::test]
async fn test_another_signer_cannot_take_over_a_session_key() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let session_key = falcon_session(&app, &state, &signer).await;

    // Another cosigner learns the key from `x-pubkey` and grants it to themselves.
    let other = TestSigner::new();
    seed_cosigner(&state, &other.commitment_hex, SignatureScheme::Falcon).await;
    let input = GrantInput::for_state(&state, &other.commitment_hex, SignatureScheme::Falcon);
    let signature = other.sign_word(input.grant(&session_key).to_word());
    let response = post_session(&app, &input.body(&session_key, "falcon", &signature)).await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "authentication_failed",
        "takeover",
    );

    // The key still acts for its original signer.
    let (status, body) = session_get(&app, &session_key, &account_id_hex).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// The grant's origin is shown to the user by the wallet; Guardian does not
/// check it against requests.
#[tokio::test]
async fn test_grant_origin_is_shown_not_enforced() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let input = GrantInput {
        origin: "https://multisig.example".to_string(),
        ..GrantInput::for_state(&state, &signer.commitment_hex, SignatureScheme::Falcon)
    };
    let session_key = falcon_session_for(&app, &input, &signer).await;
    let start = now_ms();
    for (offset, origin) in [(0, None), (1, Some("https://other.example"))] {
        let timestamp = start + offset;
        let message = request_word(
            &account_id_hex,
            timestamp,
            &json!({ "account_id": account_id_hex }),
        );
        let mut request = signed_by_session(
            "GET",
            &format!("/state?account_id={account_id_hex}"),
            &session_key,
            message,
            timestamp,
            Body::empty(),
        );
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert(header::ORIGIN, origin.parse().unwrap());
        }
        let (status, body) = send(&app, request).await;
        assert_eq!(status, StatusCode::OK, "Origin {origin:?}: {body}");
    }
}

#[tokio::test]
async fn test_rotated_guardian_key_or_network_ends_sessions() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let now = state.clock.now();
    let secs = now.timestamp() as u64;
    let current = MidenSession {
        signer_commitment: signer.commitment_hex.clone(),
        origin: String::new(),
        guardian_commitment: state.ack.commitment(&SignatureScheme::Falcon),
        network: state.dashboard.environment().to_string(),
    };

    let stale_sessions = [
        (
            "ACK key rotated since the grant",
            MidenSession {
                guardian_commitment: format!("0x{}", "11".repeat(32)),
                ..current.clone()
            },
        ),
        (
            "network changed since the grant",
            MidenSession {
                network: "elsewhere".to_string(),
                ..current.clone()
            },
        ),
    ];
    for (name, session) in stale_sessions {
        let session_key = SessionKey::generate();
        state
            .miden_sessions
            .register(&session_key.public_key(), session, secs, secs + 600, now)
            .await
            .unwrap();
        let response = session_get(&app, &session_key, &account_id_hex).await;
        assert_error(
            &response,
            StatusCode::UNAUTHORIZED,
            "authentication_failed",
            name,
        );
    }

    let session_key = SessionKey::generate();
    state
        .miden_sessions
        .register(&session_key.public_key(), current, secs, secs + 600, now)
        .await
        .unwrap();
    let (status, _) = session_get(&app, &session_key, &account_id_hex).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a grant for the current key and network"
    );
}

#[tokio::test]
async fn test_revoke_all_ends_every_session_of_the_signer() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let keys = [
        falcon_session(&app, &state, &signer).await,
        falcon_session(&app, &state, &signer).await,
    ];
    let other = TestSigner::new();
    seed_cosigner(&state, &other.commitment_hex, SignatureScheme::Falcon).await;
    let other_key = falcon_session(&app, &state, &other).await;

    let (status, body) = revoke_all(&app, &signer).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revoked"], 2);
    for key in &keys {
        let response = session_get(&app, key, &account_id_hex).await;
        assert_error(
            &response,
            StatusCode::UNAUTHORIZED,
            "session_revoked",
            "revoked by revoke-all",
        );
    }

    // Another signer's session is untouched, and revoke-all is idempotent.
    assert!(
        state
            .miden_sessions
            .find(&other_key.public_key(), state.clock.now())
            .await
            .is_ok()
    );
    let (status, body) = revoke_all(&app, &signer).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["revoked"], 0);

    // A signature by another wallet, for this signer's commitment, is rejected.
    let (signature, timestamp) = revoke_all_signature(&other);
    let response = send(
        &app,
        revoke_all_request(
            &signer.commitment_hex,
            &other.pubkey_hex,
            &signature,
            timestamp,
            None,
        ),
    )
    .await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "authentication_failed",
        "another wallet's revoke-all",
    );
}

#[tokio::test]
async fn test_replayed_revoke_all_spares_sessions_granted_after_it() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    let base = GrantInput::for_state(&state, &signer.commitment_hex, SignatureScheme::Falcon);
    let old = falcon_session_for(
        &app,
        &GrantInput {
            issued_at: base.issued_at - 10,
            ..base
        },
        &signer,
    )
    .await;

    // Revoke-all signed one second in the past; captured by a page.
    let timestamp = now_ms() - 1_000;
    let signature = signer.sign_word(revoke_all_word(&signer.commitment_hex, timestamp));
    let replay = || {
        revoke_all_request(
            &signer.commitment_hex,
            &signer.pubkey_hex,
            &signature,
            timestamp,
            None,
        )
    };
    let (status, body) = send(&app, replay()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revoked"], 1);
    let response = session_get(&app, &old, &account_id_hex).await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "session_revoked",
        "old session",
    );

    // A session granted after the revoke-all's timestamp survives its replay.
    let fresh = GrantInput {
        issued_at: (timestamp / 1000) as u64 + 1,
        ..GrantInput::for_state(&state, &signer.commitment_hex, SignatureScheme::Falcon)
    };
    let new = falcon_session_for(&app, &fresh, &signer).await;
    let (status, body) = send(&app, replay()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "replay inside the skew window: {body}"
    );
    assert_eq!(body["revoked"], 0);
    let (status, body) = session_get(&app, &new, &account_id_hex).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn test_a_grant_dated_in_the_future_cannot_outlive_revoke_all() {
    let (app, state, signer, account_id_hex) = configured_account().await;
    // A page (or a fast client clock) dates the grant ahead of the server.
    let ahead = GrantInput {
        issued_at: state.clock.now().timestamp() as u64 + 250,
        ..GrantInput::for_state(&state, &signer.commitment_hex, SignatureScheme::Falcon)
    };
    let session_key = falcon_session_for(&app, &ahead, &signer).await;

    let (status, body) = revoke_all(&app, &signer).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revoked"], 1, "registration time caps issued_at");
    let response = session_get(&app, &session_key, &account_id_hex).await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "session_revoked",
        "future-dated grant",
    );
}

#[tokio::test]
async fn test_session_cannot_lock_the_wallet_out() {
    let mut state = with_max_ttl(create_test_app_state().await);
    state.clock = Arc::new(MockClock::new(chrono::Utc::now()));
    let (app, state, signer, account_id_hex) = configured_account_with(state).await;
    let session_key = falcon_session(&app, &state, &signer).await;

    // A stolen session stamps its requests at the edge of the skew window;
    // the frozen clock keeps that edge exact.
    let now = state.clock.now().timestamp_millis();
    let ahead = now + 299_000;
    let (status, body) =
        session_get_with(&app, &session_key, "/state", &account_id_hex, ahead).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The wallet's floor is its own: a request stamped with the real time works.
    let (status, body) = wallet_get_state(&app, &signer, &account_id_hex, now_ms()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "wallet after a future session stamp: {body}"
    );

    // The session's own floor still rejects a replay of its requests.
    let response = session_get_with(&app, &session_key, "/state", &account_id_hex, ahead).await;
    assert_error(
        &response,
        StatusCode::UNAUTHORIZED,
        "authentication_replay",
        "session replay",
    );

    // And the wallet still ends the session.
    let (status, body) = revoke_all(&app, &signer).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revoked"], 1);
}
