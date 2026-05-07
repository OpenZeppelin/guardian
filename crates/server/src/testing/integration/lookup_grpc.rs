//! gRPC parity tests for `GetAccountByKeyCommitment`.
//!
//! Mirrors the coverage in `lookup_http.rs` against the gRPC method to enforce
//! transport parity (constitution principle II): same validation order, same
//! status mapping, same security properties.
//!
//! Note: tests send `x-pubkey` via `create_request_with_auth` for wire-format
//! parity with per-account requests, but the lookup verification path ignores
//! it — identity is sourced from the signature.

use crate::api::grpc::guardian::guardian_server::Guardian;
use crate::api::grpc::guardian::{AccountRef, GetAccountByKeyCommitmentRequest};
use crate::testing::helpers::{
    TestEcdsaSigner, TestSigner, create_grpc_service, create_request_with_auth,
    create_test_app_state,
};
use crate::testing::integration::lookup_helpers::{
    ecdsa_account, evm_account, falcon_account, fresh_account_id_hex, now_ms, seed, sign_lookup,
    sign_lookup_ecdsa,
};

use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::hex::FromHex;
use miden_protocol::Word;
use tonic::Code;

// --- Happy paths ---------------------------------------------------------

#[tokio::test]
async fn grpc_lookup_falcon_happy_path() {
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let account_id = fresh_account_id_hex(11);
    seed(
        &state,
        falcon_account(&account_id, vec![signer.commitment_hex.clone()]),
    )
    .await;
    let service = create_grpc_service(state);

    let timestamp = now_ms();
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);

    let req = GetAccountByKeyCommitmentRequest {
        key_commitment: signer.commitment_hex.clone(),
    };
    let request = create_request_with_auth(req, &signer.pubkey_hex, &signature, timestamp);

    let response = service
        .get_account_by_key_commitment(request)
        .await
        .expect("happy path returns OK")
        .into_inner();

    assert_eq!(response.accounts.len(), 1);
    assert_eq!(response.accounts[0].account_id, account_id);
}

#[tokio::test]
async fn grpc_lookup_ecdsa_happy_path() {
    // ECDSA-side gRPC parity. Mirrors the HTTP `lookup_ecdsa_happy_path`
    // test against the gRPC method to enforce constitution principle II
    // (transport parity) at both ECDSA and Falcon scheme layers.
    let state = create_test_app_state().await;
    let signer = TestEcdsaSigner::new();
    let account_id = fresh_account_id_hex(21);
    seed(
        &state,
        ecdsa_account(&account_id, vec![signer.commitment_hex.clone()]),
    )
    .await;
    let service = create_grpc_service(state);

    let timestamp = now_ms();
    let signature = sign_lookup_ecdsa(&signer, &signer.commitment_hex, timestamp);
    let req = GetAccountByKeyCommitmentRequest {
        key_commitment: signer.commitment_hex.clone(),
    };
    let request = create_request_with_auth(req, &signer.pubkey_hex, &signature, timestamp);

    let response = service
        .get_account_by_key_commitment(request)
        .await
        .expect("ECDSA happy path returns OK")
        .into_inner();

    assert_eq!(response.accounts.len(), 1);
    assert_eq!(response.accounts[0].account_id, account_id);
}

#[tokio::test]
async fn grpc_lookup_returns_empty_list_for_unauthorized_commitment() {
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let service = create_grpc_service(state);

    let timestamp = now_ms();
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);

    let req = GetAccountByKeyCommitmentRequest {
        key_commitment: signer.commitment_hex.clone(),
    };
    let request = create_request_with_auth(req, &signer.pubkey_hex, &signature, timestamp);

    let response = service
        .get_account_by_key_commitment(request)
        .await
        .expect("PoP succeeds even with no matches")
        .into_inner();

    assert!(
        response.accounts.is_empty(),
        "expected empty list, got {:?}",
        response.accounts
    );
}

#[tokio::test]
async fn grpc_lookup_returns_all_matches_for_shared_commitment() {
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let account_a = fresh_account_id_hex(12);
    let account_b = fresh_account_id_hex(13);
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
    let service = create_grpc_service(state);

    let timestamp = now_ms();
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);
    let req = GetAccountByKeyCommitmentRequest {
        key_commitment: signer.commitment_hex.clone(),
    };
    let request = create_request_with_auth(req, &signer.pubkey_hex, &signature, timestamp);

    let response = service
        .get_account_by_key_commitment(request)
        .await
        .expect("OK")
        .into_inner();

    let mut ids: Vec<&str> = response
        .accounts
        .iter()
        .map(|a: &AccountRef| a.account_id.as_str())
        .collect();
    ids.sort();
    let mut expected = [account_a.as_str(), account_b.as_str()];
    expected.sort();
    assert_eq!(ids, expected.to_vec());
}

#[tokio::test]
async fn grpc_lookup_excludes_evm_accounts() {
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    seed(
        &state,
        evm_account(
            &fresh_account_id_hex(14),
            vec![signer.commitment_hex.clone()],
        ),
    )
    .await;
    let service = create_grpc_service(state);

    let timestamp = now_ms();
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);
    let req = GetAccountByKeyCommitmentRequest {
        key_commitment: signer.commitment_hex.clone(),
    };
    let request = create_request_with_auth(req, &signer.pubkey_hex, &signature, timestamp);

    let response = service
        .get_account_by_key_commitment(request)
        .await
        .expect("OK")
        .into_inner();

    assert!(
        response.accounts.is_empty(),
        "EVM accounts must not match Miden-key lookups"
    );
}

// --- Negative paths ------------------------------------------------------

#[tokio::test]
async fn grpc_lookup_rejects_malformed_key_commitment_hex() {
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let service = create_grpc_service(state);

    let timestamp = now_ms();
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);
    let req = GetAccountByKeyCommitmentRequest {
        key_commitment: "0xnot-hex-at-all".to_string(),
    };
    let request = create_request_with_auth(req, &signer.pubkey_hex, &signature, timestamp);

    let err = service
        .get_account_by_key_commitment(request)
        .await
        .expect_err("malformed hex must be rejected");
    assert_eq!(err.code(), Code::InvalidArgument);
}

#[tokio::test]
async fn grpc_lookup_rejects_signature_for_different_commitment() {
    // Proof-of-possession: signing under one key but querying for another
    // commitment must fail authentication. Mirrors HTTP coverage.
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let other_signer = TestSigner::new();
    let service = create_grpc_service(state);

    assert_ne!(signer.commitment_hex, other_signer.commitment_hex);

    let timestamp = now_ms();
    let signature = sign_lookup(&signer, &other_signer.commitment_hex, timestamp);
    let req = GetAccountByKeyCommitmentRequest {
        key_commitment: other_signer.commitment_hex.clone(),
    };
    let request = create_request_with_auth(req, &signer.pubkey_hex, &signature, timestamp);

    let err = service
        .get_account_by_key_commitment(request)
        .await
        .expect_err("mismatch must be rejected");
    assert_eq!(err.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn grpc_lookup_rejects_tampered_signature() {
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let service = create_grpc_service(state);

    let timestamp = now_ms();
    let signature = sign_lookup(&signer, &signer.commitment_hex, timestamp);
    let mut tampered = signature.into_bytes();
    let mid = tampered.len() / 2;
    tampered[mid] = if tampered[mid] == b'0' { b'1' } else { b'0' };
    let tampered_hex = String::from_utf8(tampered).unwrap();

    let req = GetAccountByKeyCommitmentRequest {
        key_commitment: signer.commitment_hex.clone(),
    };
    let request = create_request_with_auth(req, &signer.pubkey_hex, &tampered_hex, timestamp);

    let err = service
        .get_account_by_key_commitment(request)
        .await
        .expect_err("tampered sig must be rejected");
    assert_eq!(err.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn grpc_lookup_rejects_timestamp_outside_skew_window() {
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let service = create_grpc_service(state);

    let stale_timestamp = now_ms() - 10 * 60 * 1000;
    let signature = sign_lookup(&signer, &signer.commitment_hex, stale_timestamp);
    let req = GetAccountByKeyCommitmentRequest {
        key_commitment: signer.commitment_hex.clone(),
    };
    let request = create_request_with_auth(req, &signer.pubkey_hex, &signature, stale_timestamp);

    let err = service
        .get_account_by_key_commitment(request)
        .await
        .expect_err("stale timestamp must be rejected");
    assert_eq!(err.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn grpc_lookup_rejects_signature_signed_for_auth_request_message() {
    // Cross-domain replay over gRPC: the same domain-separation property as
    // HTTP must hold. A signature crafted for AuthRequestMessage MUST NOT
    // validate against the lookup RPC.
    let state = create_test_app_state().await;
    let signer = TestSigner::new();
    let account_id = fresh_account_id_hex(15);
    seed(
        &state,
        falcon_account(&account_id, vec![signer.commitment_hex.clone()]),
    )
    .await;
    let service = create_grpc_service(state);

    let timestamp = now_ms();
    let key_commitment_word =
        Word::from_hex(&signer.commitment_hex).expect("commitment is valid Word");
    let payload = AuthRequestPayload::from_bytes(&key_commitment_word.as_bytes());
    let request_digest = AuthRequestMessage::from_account_id_hex(&account_id, timestamp, payload)
        .expect("valid account id")
        .to_word();
    let bad_signature = signer.sign_word(request_digest);

    let req = GetAccountByKeyCommitmentRequest {
        key_commitment: signer.commitment_hex.clone(),
    };
    let request = create_request_with_auth(req, &signer.pubkey_hex, &bad_signature, timestamp);

    let err = service
        .get_account_by_key_commitment(request)
        .await
        .expect_err("cross-domain replay must be rejected");
    assert_eq!(err.code(), Code::Unauthenticated);
}
