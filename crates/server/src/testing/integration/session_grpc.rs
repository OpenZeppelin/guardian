//! gRPC integration tests for Miden account sessions.

use super::lookup_helpers::{lookup_digest, now_ms};
use super::session_helpers::{
    GrantInput, logout_signature, revoke_all_signature, revoke_all_word, seed_cosigner,
    with_max_ttl,
};
use crate::api::grpc::guardian::guardian_server::Guardian;
use crate::api::grpc::guardian::{
    AbandonDeltaCandidateRequest, ConfigureRequest, CreateSessionRequest,
    GetAccountByKeyCommitmentRequest, GetCanonicalNonceRequest, GetDeltaHistoryRequest,
    GetDeltaProposalRequest, GetDeltaProposalsRequest, GetDeltaRequest, GetDeltaSinceRequest,
    GetStateRequest, PushDeltaRequest, RevokeAllSessionsRequest, RevokeSessionRequest,
    SessionGrantFields,
};
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::session::MidenSession;
use crate::state::AppState;
use crate::testing::helpers::{
    IntegrationMockNetworkClient, TestSigner, create_grpc_service, create_miden_falcon_rpo_auth,
    create_miden_network_config, create_request_with_auth, create_signed_request_with_auth,
    create_test_app_state, fixture_signer, load_fixture_account_grpc as load_fixture_account,
    load_fixture_delta,
};
use guardian_shared::SignatureScheme;
use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::session_key::SessionKey;
use miden_protocol::Word;
use prost::Message;
use std::sync::Arc;
use tonic::{Code, Request, Status};

/// Marks a request as signed by a delegated signer.
fn as_session<T>(mut request: Request<T>) -> Request<T> {
    request
        .metadata_mut()
        .insert("x-auth-format", "session".parse().unwrap());
    request
}

/// A request signed by `key` over `message` at `timestamp`.
fn signed_by_session<T>(message: T, key: &SessionKey, word: Word, timestamp: i64) -> Request<T> {
    as_session(create_request_with_auth(
        message,
        &key.public_key_hex(),
        &key.sign_hex(word),
        timestamp,
    ))
}

/// An account request signed by `key` over its `AuthRequestMessage`.
fn session_request<T: Message>(message: T, account_id_hex: &str, key: &SessionKey) -> Request<T> {
    let timestamp = now_ms();
    let payload = AuthRequestPayload::from_protobuf_message(&message);
    let word = AuthRequestMessage::from_account_id_hex(account_id_hex, timestamp, payload)
        .unwrap()
        .to_word();
    signed_by_session(message, key, word, timestamp)
}

fn assert_error(status: &Status, code: Code, guardian_code: &str, context: &str) {
    assert_eq!(status.code(), code, "{context}: {}", status.message());
    let details: serde_json::Value =
        serde_json::from_slice(status.details()).expect("Status.details is JSON");
    assert_eq!(details["code"], guardian_code, "{context}");
}

/// Registers a session for the Falcon `signer` and returns its key.
async fn falcon_session(
    service: &crate::api::grpc::GuardianService,
    state: &AppState,
    signer: &TestSigner,
) -> SessionKey {
    let key = SessionKey::generate();
    let input = GrantInput::for_state(state, &signer.commitment_hex, SignatureScheme::Falcon);
    let created = service
        .create_session(Request::new(CreateSessionRequest {
            scheme: "falcon".to_string(),
            auth_format: None,
            public_key: None,
            signature: signer.sign_word(input.grant(&key).to_word()),
            grant: Some(SessionGrantFields {
                signer_commitment: input.signer_commitment.clone(),
                session_public_key: key.public_key_hex(),
                origin: input.origin.clone(),
                issued_at: input.issued_at,
                expires_at: input.expires_at,
                guardian_commitment: input.guardian_commitment.clone(),
                network: input.network.clone(),
            }),
        }))
        .await
        .expect("create_session")
        .into_inner();
    assert_eq!(created.signer_commitment, signer.commitment_hex);
    key
}

/// The fixture account configured by its first cosigner.
async fn configured_account() -> (
    crate::api::grpc::GuardianService,
    AppState,
    TestSigner,
    String,
) {
    let mut state = with_max_ttl(create_test_app_state().await);
    state.network_client = Arc::new(IntegrationMockNetworkClient::new(
        MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal),
    ));
    let service = create_grpc_service(state.clone());

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let (signer, cosigner_commitments) = fixture_signer();
    service
        .configure(create_signed_request_with_auth(
            ConfigureRequest {
                account_id: account_id_hex.clone(),
                auth: Some(create_miden_falcon_rpo_auth(cosigner_commitments)),
                network_config: Some(create_miden_network_config()),
                initial_state,
            },
            &account_id_hex,
            &signer,
        ))
        .await
        .expect("configure");
    (service, state, signer, account_id_hex)
}

/// Calls every wallet-only RPC with credentials of `key` and asserts the
/// resulting status and Guardian code.
async fn assert_wallet_only_rpcs(
    service: &crate::api::grpc::GuardianService,
    signer: &TestSigner,
    account_id_hex: &str,
    key: &SessionKey,
    code: Code,
    guardian_code: &str,
) {
    let (_account_id, _, initial_state) = load_fixture_account();
    let delta = load_fixture_delta(1);

    let status = service
        .configure(session_request(
            ConfigureRequest {
                account_id: account_id_hex.to_string(),
                auth: Some(create_miden_falcon_rpo_auth(vec![
                    signer.commitment_hex.clone(),
                ])),
                network_config: Some(create_miden_network_config()),
                initial_state,
            },
            account_id_hex,
            key,
        ))
        .await
        .expect_err("configure is wallet-only");
    assert_error(&status, code, guardian_code, "Configure");

    let status = service
        .push_delta(session_request(
            PushDeltaRequest {
                account_id: account_id_hex.to_string(),
                nonce: 1,
                prev_commitment: delta["prev_commitment"].as_str().unwrap().to_string(),
                delta_payload: delta["delta_payload"].to_string(),
            },
            account_id_hex,
            key,
        ))
        .await
        .expect_err("push_delta is wallet-only");
    assert_error(&status, code, guardian_code, "PushDelta");

    // AbandonDeltaCandidate reports errors in its response body.
    let response = service
        .abandon_delta_candidate(session_request(
            AbandonDeltaCandidateRequest {
                account_id: account_id_hex.to_string(),
                nonce: 1,
            },
            account_id_hex,
            key,
        ))
        .await
        .expect("abandon answers in its body")
        .into_inner();
    assert!(!response.success, "AbandonDeltaCandidate");
    assert_eq!(response.error_code, guardian_code, "AbandonDeltaCandidate");

    // Account-less routes: the session signs the route's own message.
    let timestamp = now_ms();
    let status = service
        .get_account_by_key_commitment(signed_by_session(
            GetAccountByKeyCommitmentRequest {
                key_commitment: signer.commitment_hex.clone(),
            },
            key,
            lookup_digest(&signer.commitment_hex, timestamp),
            timestamp,
        ))
        .await
        .expect_err("lookup is wallet-only");
    assert_error(&status, code, guardian_code, "GetAccountByKeyCommitment");

    let status = service
        .revoke_all_sessions(signed_by_session(
            RevokeAllSessionsRequest {
                signer_commitment: signer.commitment_hex.clone(),
            },
            key,
            revoke_all_word(&signer.commitment_hex, timestamp),
            timestamp,
        ))
        .await
        .expect_err("revoke-all is wallet-only");
    assert_error(&status, code, guardian_code, "RevokeAllSessions");
}

#[tokio::test]
async fn test_grpc_session_lifecycle() {
    let (service, state, signer, account_id_hex) = configured_account().await;
    let key = falcon_session(&service, &state, &signer).await;

    let get_state = || GetStateRequest {
        account_id: account_id_hex.clone(),
    };
    let response = service
        .get_state(session_request(get_state(), &account_id_hex, &key))
        .await
        .expect("session-signed get_state")
        .into_inner();
    assert!(response.success);

    let (signature, timestamp) = logout_signature(&key);
    let logout = create_request_with_auth(
        RevokeSessionRequest {},
        &key.public_key_hex(),
        &signature,
        timestamp,
    );
    assert!(
        service
            .revoke_session(logout)
            .await
            .expect("revoke")
            .into_inner()
            .revoked
    );

    let status = service
        .get_state(session_request(get_state(), &account_id_hex, &key))
        .await
        .expect_err("revoked session must be rejected");
    assert_error(
        &status,
        Code::Unauthenticated,
        "session_revoked",
        "after logout",
    );
}

#[tokio::test]
async fn test_grpc_wallet_only_routes_reject_the_session() {
    let (service, state, signer, account_id_hex) = configured_account().await;
    let key = falcon_session(&service, &state, &signer).await;

    assert_wallet_only_rpcs(
        &service,
        &signer,
        &account_id_hex,
        &key,
        Code::PermissionDenied,
        "wallet_signature_required",
    )
    .await;

    // The session is untouched by the rejected calls.
    assert!(
        service
            .get_state(session_request(
                GetStateRequest {
                    account_id: account_id_hex.clone(),
                },
                &account_id_hex,
                &key,
            ))
            .await
            .expect("session still valid")
            .into_inner()
            .success
    );
}

#[tokio::test]
async fn test_grpc_forged_session_credentials_fail_authentication() {
    let (service, _state, signer, account_id_hex) = configured_account().await;
    assert_wallet_only_rpcs(
        &service,
        &signer,
        &account_id_hex,
        &SessionKey::generate(),
        Code::Unauthenticated,
        "authentication_failed",
    )
    .await;
}

#[tokio::test]
async fn test_grpc_revoke_all_ends_every_session_of_the_signer() {
    let (service, state, signer, account_id_hex) = configured_account().await;
    let keys = [
        falcon_session(&service, &state, &signer).await,
        falcon_session(&service, &state, &signer).await,
    ];

    let revoke_all = || {
        let (signature, timestamp) = revoke_all_signature(&signer);
        create_request_with_auth(
            RevokeAllSessionsRequest {
                signer_commitment: signer.commitment_hex.clone(),
            },
            &signer.pubkey_hex,
            &signature,
            timestamp,
        )
    };
    let revoked = service
        .revoke_all_sessions(revoke_all())
        .await
        .expect("revoke-all")
        .into_inner()
        .revoked;
    assert_eq!(revoked, 2);

    for key in &keys {
        let status = service
            .get_state(session_request(
                GetStateRequest {
                    account_id: account_id_hex.clone(),
                },
                &account_id_hex,
                key,
            ))
            .await
            .expect_err("revoked session must be rejected");
        assert_error(
            &status,
            Code::Unauthenticated,
            "session_revoked",
            "after revoke-all",
        );
    }

    let revoked = service
        .revoke_all_sessions(revoke_all())
        .await
        .expect("revoke-all is idempotent")
        .into_inner()
        .revoked;
    assert_eq!(revoked, 0);
}

/// Every FR-008 read RPC accepts the session: none of them answers with an
/// authentication or authorization error.
#[tokio::test]
async fn test_grpc_every_session_eligible_read_accepts_the_session() {
    let (service, state, signer, account_id_hex) = configured_account().await;
    let key = falcon_session(&service, &state, &signer).await;
    let account_id = || account_id_hex.clone();
    let refused = |result: Result<(), Status>, rpc: &str| {
        if let Err(status) = result {
            assert!(
                !matches!(
                    status.code(),
                    Code::Unauthenticated | Code::PermissionDenied
                ),
                "{rpc} refused the session: {status:?}"
            );
        }
    };

    refused(
        service
            .get_delta(session_request(
                GetDeltaRequest {
                    account_id: account_id(),
                    nonce: 1,
                },
                &account_id_hex,
                &key,
            ))
            .await
            .map(drop),
        "GetDelta",
    );
    refused(
        service
            .get_delta_since(session_request(
                GetDeltaSinceRequest {
                    account_id: account_id(),
                    from_nonce: 1,
                },
                &account_id_hex,
                &key,
            ))
            .await
            .map(drop),
        "GetDeltaSince",
    );
    refused(
        service
            .get_delta_history(session_request(
                GetDeltaHistoryRequest {
                    account_id: account_id(),
                    limit: None,
                    cursor: None,
                },
                &account_id_hex,
                &key,
            ))
            .await
            .map(drop),
        "GetDeltaHistory",
    );
    refused(
        service
            .get_canonical_nonce(session_request(
                GetCanonicalNonceRequest {
                    account_id: account_id(),
                },
                &account_id_hex,
                &key,
            ))
            .await
            .map(drop),
        "GetCanonicalNonce",
    );
    refused(
        service
            .get_delta_proposals(session_request(
                GetDeltaProposalsRequest {
                    account_id: account_id(),
                },
                &account_id_hex,
                &key,
            ))
            .await
            .map(drop),
        "GetDeltaProposals",
    );
    refused(
        service
            .get_delta_proposal(session_request(
                GetDeltaProposalRequest {
                    account_id: account_id(),
                    commitment: format!("0x{}", "ab".repeat(32)),
                },
                &account_id_hex,
                &key,
            ))
            .await
            .map(drop),
        "GetDeltaProposal",
    );
}

/// FR-007 order on gRPC: wallet-only RPCs answer `wallet_signature_required`
/// before the grant's Guardian key and cosigner checks.
#[tokio::test]
async fn test_grpc_wallet_only_rpcs_answer_before_the_grant_checks() {
    let (service, state, signer, account_id_hex) = configured_account().await;
    let outsider = TestSigner::new();
    seed_cosigner(&state, &outsider.commitment_hex, SignatureScheme::Falcon).await;
    let not_a_cosigner = falcon_session(&service, &state, &outsider).await;
    let rotated_key = SessionKey::generate();
    let now = state.clock.now();
    let secs = now.timestamp() as u64;
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
    let delta = load_fixture_delta(1);

    for (name, key) in [
        ("signer does not cosign the account", &not_a_cosigner),
        ("grant names a rotated Guardian key", &rotated_key),
    ] {
        let status = service
            .push_delta(session_request(
                PushDeltaRequest {
                    account_id: account_id_hex.clone(),
                    nonce: 1,
                    prev_commitment: delta["prev_commitment"].as_str().unwrap().to_string(),
                    delta_payload: delta["delta_payload"].to_string(),
                },
                &account_id_hex,
                key,
            ))
            .await
            .expect_err("push_delta is wallet-only");
        assert_error(
            &status,
            Code::PermissionDenied,
            "wallet_signature_required",
            &format!("PushDelta: {name}"),
        );

        let response = service
            .abandon_delta_candidate(session_request(
                AbandonDeltaCandidateRequest {
                    account_id: account_id_hex.clone(),
                    nonce: 1,
                },
                &account_id_hex,
                key,
            ))
            .await
            .expect("abandon answers in its body")
            .into_inner();
        assert_eq!(
            response.error_code, "wallet_signature_required",
            "AbandonDeltaCandidate: {name}"
        );
    }
}
