//! Mixed Falcon + ECDSA accounts over the HTTP transport (issue #563): the
//! per-signer `Miden` auth on `POST /configure`, request authentication
//! under each cosigner's own scheme, `GET /state/lookup`, and delegated
//! sessions granted by either cosigner.

use super::lookup_helpers::{lookup_digest, now_ms};
use super::session_helpers::{GrantInput, with_max_ttl};
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::session::MidenSession;
use crate::state::AppState;
use crate::testing::helpers::{
    TestEcdsaSigner, TestSigner, build_guarded_multisig_account, create_router,
    create_test_app_state, word_from_commitment_hex,
};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use guardian_shared::SignatureScheme;
use guardian_shared::ToJson;
use guardian_shared::auth_request_eip712::request_digest;
use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::session_key::SessionKey;
use miden_protocol::Word;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::Service;

struct MixedAccount {
    app: axum::Router,
    state: AppState,
    falcon: TestSigner,
    ecdsa: TestEcdsaSigner,
    account_id_hex: String,
}

async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().call(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn assert_error(response: &(StatusCode, Value), status: StatusCode, code: &str, context: &str) {
    assert_eq!(response.0, status, "{context}: {}", response.1);
    assert_eq!(response.1["code"], code, "{context}: {}", response.1);
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

fn post_json(
    uri: &str,
    body: &Value,
    pubkey: &str,
    signature: &str,
    timestamp: i64,
) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", pubkey)
        .header("x-signature", signature)
        .header("x-timestamp", timestamp.to_string())
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get_signed(
    uri: &str,
    pubkey: &str,
    signature: &str,
    timestamp: i64,
    auth_format: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .uri(uri)
        .method("GET")
        .header("x-pubkey", pubkey)
        .header("x-signature", signature)
        .header("x-timestamp", timestamp.to_string());
    if let Some(format) = auth_format {
        builder = builder.header("x-auth-format", format);
    }
    builder.body(Body::empty()).unwrap()
}

fn state_query(account_id_hex: &str) -> (String, Value) {
    (
        format!("/state?account_id={account_id_hex}"),
        json!({ "account_id": account_id_hex }),
    )
}

/// `GET /state` signed raw by the Falcon cosigner.
async fn get_state_as_falcon(fixture: &MixedAccount, signer: &TestSigner) -> (StatusCode, Value) {
    let (uri, payload) = state_query(&fixture.account_id_hex);
    let timestamp = now_ms();
    let signature = signer.sign_word(request_word(&fixture.account_id_hex, timestamp, &payload));
    send(
        &fixture.app,
        get_signed(&uri, &signer.pubkey_hex, &signature, timestamp, None),
    )
    .await
}

/// `GET /state` signed by an ECDSA key, raw or as EIP-712 typed data.
async fn get_state_as_ecdsa(
    fixture: &MixedAccount,
    signer: &TestEcdsaSigner,
    eip712: bool,
) -> (StatusCode, Value) {
    let (uri, payload) = state_query(&fixture.account_id_hex);
    let timestamp = now_ms();
    let word = request_word(&fixture.account_id_hex, timestamp, &payload);
    let (signature, format) = if eip712 {
        (signer.sign_prehash(request_digest(word)), Some("eip712"))
    } else {
        (signer.sign_word(word), None)
    };
    send(
        &fixture.app,
        get_signed(&uri, &signer.pubkey_hex, &signature, timestamp, format),
    )
    .await
}

/// A Falcon + ECDSA account with an ECDSA GUARDIAN (this server's ECDSA ACK
/// key), registered over HTTP with the per-signer auth, signed by the
/// Falcon cosigner. The real network client decodes the state, so the
/// GUARDIAN binding (key and scheme) and the signer set are really checked.
async fn configured_mixed_account() -> MixedAccount {
    let mut state = with_max_ttl(create_test_app_state().await);
    state.network_client = Arc::new(MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal));
    let app = create_router(state.clone());

    let falcon = TestSigner::new();
    let ecdsa = TestEcdsaSigner::new();
    let account = build_guarded_multisig_account(
        [17u8; 32],
        2,
        &[
            (
                word_from_commitment_hex(&falcon.commitment_hex),
                SignatureScheme::Falcon,
            ),
            (
                word_from_commitment_hex(&ecdsa.commitment_hex),
                SignatureScheme::Ecdsa,
            ),
        ],
        (
            word_from_commitment_hex(&state.ack.commitment(&SignatureScheme::Ecdsa)),
            SignatureScheme::Ecdsa,
        ),
    );
    let account_id_hex = account.id().to_hex();

    let body = json!({
        "account_id": account_id_hex,
        "auth": { "Miden": {
            "cosigners": [
                { "commitment": falcon.commitment_hex, "scheme": "falcon" },
                { "commitment": ecdsa.commitment_hex, "scheme": "ecdsa" }
            ],
            "guardian_scheme": "ecdsa"
        } },
        "initial_state": account.to_json(),
    });
    let (signature, timestamp) = falcon.sign_json_payload(&account_id_hex, &body);
    let (status, response) = send(
        &app,
        post_json(
            "/configure",
            &body,
            &falcon.pubkey_hex,
            &signature,
            timestamp,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "configure: {response}");
    assert_eq!(
        response["ack_commitment"],
        state.ack.commitment(&SignatureScheme::Ecdsa),
        "acknowledged with the GUARDIAN's ECDSA key: {response}"
    );

    MixedAccount {
        app,
        state,
        falcon,
        ecdsa,
        account_id_hex,
    }
}

#[tokio::test]
async fn each_cosigner_authenticates_under_its_own_scheme() {
    let fixture = configured_mixed_account().await;

    let (status, body) = get_state_as_falcon(&fixture, &fixture.falcon).await;
    assert_eq!(status, StatusCode::OK, "Falcon raw: {body}");
    assert_eq!(body["auth_scheme"], "mixed");
    let (status, body) = get_state_as_ecdsa(&fixture, &fixture.ecdsa, false).await;
    assert_eq!(status, StatusCode::OK, "ECDSA raw: {body}");
    let (status, body) = get_state_as_ecdsa(&fixture, &fixture.ecdsa, true).await;
    assert_eq!(status, StatusCode::OK, "ECDSA EIP-712: {body}");

    // Keys that are not cosigners, of either scheme.
    assert_error(
        &get_state_as_falcon(&fixture, &TestSigner::new()).await,
        StatusCode::UNAUTHORIZED,
        "authentication_failed",
        "unregistered Falcon key",
    );
    assert_error(
        &get_state_as_ecdsa(&fixture, &TestEcdsaSigner::new(), false).await,
        StatusCode::UNAUTHORIZED,
        "authentication_failed",
        "unregistered ECDSA key",
    );

    // A signature of neither scheme's length.
    let (uri, _) = state_query(&fixture.account_id_hex);
    let truncated = format!("0x{}", "11".repeat(64));
    assert_error(
        &send(
            &fixture.app,
            get_signed(&uri, &fixture.ecdsa.pubkey_hex, &truncated, now_ms(), None),
        )
        .await,
        StatusCode::UNAUTHORIZED,
        "authentication_failed",
        "64-byte signature",
    );
}

#[tokio::test]
async fn single_scheme_declaration_of_a_mixed_state_is_refused() {
    let state = create_test_app_state().await;
    let mut state = state;
    state.network_client = Arc::new(MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal));
    let app = create_router(state.clone());
    let falcon = TestSigner::new();
    let ecdsa = TestEcdsaSigner::new();
    let account = build_guarded_multisig_account(
        [18u8; 32],
        1,
        &[
            (
                word_from_commitment_hex(&falcon.commitment_hex),
                SignatureScheme::Falcon,
            ),
            (
                word_from_commitment_hex(&ecdsa.commitment_hex),
                SignatureScheme::Ecdsa,
            ),
        ],
        (
            word_from_commitment_hex(&state.ack.commitment(&SignatureScheme::Ecdsa)),
            SignatureScheme::Ecdsa,
        ),
    );
    let account_id_hex = account.id().to_hex();
    let body = json!({
        "account_id": account_id_hex,
        "auth": { "MidenEcdsa": { "cosigner_commitments": [falcon.commitment_hex, ecdsa.commitment_hex] } },
        "initial_state": account.to_json(),
    });
    let (signature, timestamp) = ecdsa.sign_json_payload(&account_id_hex, &body);
    let response = send(
        &app,
        post_json(
            "/configure",
            &body,
            &ecdsa.pubkey_hex,
            &signature,
            timestamp,
        ),
    )
    .await;
    // The envelope carries the stable code only; the service-level tests
    // pin the message naming the schemes.
    assert_error(
        &response,
        StatusCode::BAD_REQUEST,
        "invalid_input",
        "MidenEcdsa declaration",
    );
    assert!(
        state.metadata.get(&account_id_hex).await.unwrap().is_none(),
        "nothing is stored for a refused declaration"
    );

    // An unknown scheme name is malformed input.
    let body = json!({
        "account_id": account_id_hex,
        "auth": { "Miden": {
            "cosigners": [{ "commitment": ecdsa.commitment_hex, "scheme": "rsa" }],
            "guardian_scheme": "ecdsa"
        } },
        "initial_state": account.to_json(),
    });
    let (signature, timestamp) = ecdsa.sign_json_payload(&account_id_hex, &body);
    let (status, response) = send(
        &app,
        post_json(
            "/configure",
            &body,
            &ecdsa.pubkey_hex,
            &signature,
            timestamp,
        ),
    )
    .await;
    assert!(
        status.is_client_error(),
        "unknown scheme: {status} {response}"
    );
}

#[tokio::test]
async fn both_cosigners_look_up_the_mixed_account() {
    let fixture = configured_mixed_account().await;
    let uri = |commitment: &str| format!("/state/lookup?key_commitment={commitment}");

    let timestamp = now_ms();
    let signature = fixture
        .falcon
        .sign_word(lookup_digest(&fixture.falcon.commitment_hex, timestamp));
    let (status, body) = send(
        &fixture.app,
        get_signed(
            &uri(&fixture.falcon.commitment_hex),
            &fixture.falcon.pubkey_hex,
            &signature,
            timestamp,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "Falcon lookup: {body}");
    assert_eq!(
        body["accounts"][0]["account_id"], fixture.account_id_hex,
        "{body}"
    );

    let timestamp = now_ms();
    let signature = fixture
        .ecdsa
        .sign_word(lookup_digest(&fixture.ecdsa.commitment_hex, timestamp));
    let (status, body) = send(
        &fixture.app,
        get_signed(
            &uri(&fixture.ecdsa.commitment_hex),
            &fixture.ecdsa.pubkey_hex,
            &signature,
            timestamp,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "ECDSA lookup: {body}");
    assert_eq!(
        body["accounts"][0]["account_id"], fixture.account_id_hex,
        "{body}"
    );
}

/// `GET /state` signed by a session key.
async fn get_state_with_session(
    fixture: &MixedAccount,
    session_key: &SessionKey,
) -> (StatusCode, Value) {
    let (uri, payload) = state_query(&fixture.account_id_hex);
    let timestamp = now_ms();
    let message = request_word(&fixture.account_id_hex, timestamp, &payload);
    let request = Request::builder()
        .uri(uri)
        .method("GET")
        .header("x-pubkey", session_key.public_key_hex())
        .header("x-signature", session_key.sign_hex(message))
        .header("x-timestamp", timestamp.to_string())
        .header("x-auth-format", "session")
        .body(Body::empty())
        .unwrap();
    send(&fixture.app, request).await
}

async fn post_session(fixture: &MixedAccount, body: &Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .uri("/session")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    send(&fixture.app, request).await
}

/// Each cosigner grants a session under its own scheme, and it reaches the
/// account; a session naming the ACK key of the other scheme does not.
#[tokio::test]
async fn sessions_follow_each_cosigners_scheme() {
    let fixture = configured_mixed_account().await;

    let falcon_key = SessionKey::generate();
    let input = GrantInput::for_state(
        &fixture.state,
        &fixture.falcon.commitment_hex,
        SignatureScheme::Falcon,
    );
    let signature = fixture.falcon.sign_word(input.grant(&falcon_key).to_word());
    let (status, body) =
        post_session(&fixture, &input.body(&falcon_key, "falcon", &signature)).await;
    assert_eq!(status, StatusCode::OK, "Falcon grant: {body}");
    let (status, body) = get_state_with_session(&fixture, &falcon_key).await;
    assert_eq!(status, StatusCode::OK, "Falcon session: {body}");

    let ecdsa_key = SessionKey::generate();
    let input = GrantInput::for_state(
        &fixture.state,
        &fixture.ecdsa.commitment_hex,
        SignatureScheme::Ecdsa,
    );
    let signature = fixture.ecdsa.sign_word(input.grant(&ecdsa_key).to_word());
    let mut grant = input.body(&ecdsa_key, "ecdsa", &signature);
    grant["public_key"] = json!(fixture.ecdsa.pubkey_hex);
    let (status, body) = post_session(&fixture, &grant).await;
    assert_eq!(status, StatusCode::OK, "ECDSA grant: {body}");
    let (status, body) = get_state_with_session(&fixture, &ecdsa_key).await;
    assert_eq!(status, StatusCode::OK, "ECDSA session: {body}");

    // The ECDSA cosigner's session bound to this server's Falcon key, as a
    // grant for a Falcon account would be, cannot reach this account.
    let now = fixture.state.clock.now();
    let secs = now.timestamp() as u64;
    let crossed = SessionKey::generate();
    fixture
        .state
        .miden_sessions
        .register(
            &crossed.public_key(),
            MidenSession {
                signer_commitment: fixture.ecdsa.commitment_hex.clone(),
                origin: String::new(),
                guardian_commitment: fixture.state.ack.commitment(&SignatureScheme::Falcon),
                network: fixture.state.dashboard.environment().to_string(),
            },
            secs,
            secs + 600,
            now,
        )
        .await
        .unwrap();
    assert_error(
        &get_state_with_session(&fixture, &crossed).await,
        StatusCode::FORBIDDEN,
        "authorization_failed",
        "session for the other scheme",
    );
}
