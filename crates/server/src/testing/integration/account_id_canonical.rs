//! `Configure` accepts a Miden account id only in its canonical form, the
//! lowercase hex of `AccountId::to_hex`, over HTTP and gRPC.

use crate::api::grpc::guardian::guardian_server::Guardian;
use crate::api::grpc::guardian::{
    AuthConfig, ConfigureRequest, MidenAuth, MidenCosigner, auth_config,
};
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::state::AppState;
use crate::testing::helpers::{
    TestSigner, build_guarded_multisig_account, create_grpc_service, create_miden_network_config,
    create_router, create_signed_request_with_auth, create_test_app_state,
    word_from_commitment_hex,
};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use guardian_shared::SignatureScheme;
use guardian_shared::ToJson;
use miden_protocol::account::{Account, AccountId};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::Service;

async fn test_state() -> AppState {
    let mut state = create_test_app_state().await;
    state.network_client = Arc::new(MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal));
    state
}

fn fixture_account(state: &AppState, signer: &TestSigner, seed: [u8; 32]) -> Account {
    build_guarded_multisig_account(
        seed,
        1,
        &[(
            word_from_commitment_hex(&signer.commitment_hex),
            SignatureScheme::Falcon,
        )],
        (
            word_from_commitment_hex(&state.ack.commitment(&SignatureScheme::Falcon)),
            SignatureScheme::Falcon,
        ),
    )
}

fn non_canonical_form(account_id_hex: &str) -> String {
    let position = account_id_hex
        .char_indices()
        .skip(2)
        .find(|(_, c)| matches!(c, 'a'..='f'))
        .map(|(index, _)| index)
        .expect("the account id has a hex letter");
    let mut other = account_id_hex.to_string();
    other.replace_range(
        position..=position,
        &account_id_hex[position..=position].to_uppercase(),
    );
    assert_eq!(
        AccountId::from_hex(&other).expect("the id still parses"),
        AccountId::from_hex(account_id_hex).expect("the canonical id parses"),
    );
    other
}

async fn http_configure(
    app: &axum::Router,
    signer: &TestSigner,
    account_id_hex: &str,
    account: &Account,
) -> (StatusCode, Value) {
    let body = json!({
        "account_id": account_id_hex,
        "auth": { "Miden": {
            "cosigners": [{ "commitment": signer.commitment_hex, "scheme": "falcon" }],
            "guardian_scheme": "falcon"
        } },
        "initial_state": account.to_json(),
    });
    let (signature, timestamp) = signer.sign_json_payload(account_id_hex, &body);
    let request = Request::builder()
        .uri("/configure")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-pubkey", &signer.pubkey_hex)
        .header("x-signature", &signature)
        .header("x-timestamp", timestamp.to_string())
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().call(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn has_record(state: &AppState, account_id: &str) -> bool {
    state
        .metadata
        .get(account_id)
        .await
        .expect("metadata read succeeds")
        .is_some()
}

#[tokio::test]
async fn http_configure_accepts_only_the_canonical_account_id() {
    let state = test_state().await;
    let app = create_router(state.clone());
    let signer = TestSigner::new();
    let account = fixture_account(&state, &signer, [23u8; 32]);
    let canonical = account.id().to_hex();
    let other = non_canonical_form(&canonical);

    let (status, body) = http_configure(&app, &signer, &other, &account).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "non-canonical: {body}");
    assert_eq!(body["code"], "invalid_account_id", "non-canonical: {body}");
    assert!(!has_record(&state, &other).await);

    let (status, body) = http_configure(&app, &signer, &canonical, &account).await;
    assert_eq!(status, StatusCode::OK, "canonical: {body}");
    assert!(has_record(&state, &canonical).await);

    let (status, body) = http_configure(&app, &signer, &other, &account).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "non-canonical: {body}");
    assert_eq!(body["code"], "invalid_account_id", "non-canonical: {body}");
    assert!(!has_record(&state, &other).await);
}

#[tokio::test]
async fn grpc_configure_accepts_only_the_canonical_account_id() {
    let state = test_state().await;
    let service = create_grpc_service(state.clone());
    let signer = TestSigner::new();
    let account = fixture_account(&state, &signer, [29u8; 32]);
    let other = non_canonical_form(&account.id().to_hex());

    let request = ConfigureRequest {
        account_id: other.clone(),
        auth: Some(AuthConfig {
            auth_type: Some(auth_config::AuthType::Miden(MidenAuth {
                cosigners: vec![MidenCosigner {
                    commitment: signer.commitment_hex.clone(),
                    scheme: "falcon".to_string(),
                }],
                guardian_scheme: "falcon".to_string(),
            })),
        }),
        network_config: Some(create_miden_network_config()),
        initial_state: serde_json::to_string(&account.to_json()).unwrap(),
    };
    let status = service
        .configure(create_signed_request_with_auth(request, &other, &signer))
        .await
        .expect_err("a non-canonical account id is refused");

    assert_eq!(status.code(), tonic::Code::InvalidArgument, "{status:?}");
    assert!(
        String::from_utf8_lossy(status.details()).contains("invalid_account_id"),
        "{status:?}"
    );
    assert!(!has_record(&state, &other).await);
}
