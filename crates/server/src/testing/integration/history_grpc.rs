//! gRPC integration tests for `GetHistory` (issue #413).

use crate::api::grpc::guardian::guardian_server::Guardian;
use crate::api::grpc::guardian::{ConfigureRequest, GetHistoryRequest};
use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::testing::helpers::{
    TestSigner, create_grpc_service, create_miden_falcon_rpo_auth, create_miden_network_config,
    create_signed_request_with_auth, create_test_app_state,
    load_fixture_account_grpc as load_fixture_account,
};

async fn configured_service() -> (
    crate::state::AppState,
    crate::api::grpc::GuardianService,
    TestSigner,
    String,
) {
    let state = create_test_app_state().await;
    let service = create_grpc_service(state.clone());

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let signer = TestSigner::new();

    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(vec![
            signer.commitment_hex.clone(),
        ])),
        network_config: Some(create_miden_network_config()),
        initial_state,
    };
    let response = service
        .configure(create_signed_request_with_auth(
            configure_req,
            &account_id_hex,
            &signer,
        ))
        .await
        .expect("configure should succeed");
    assert!(response.into_inner().success);

    (state, service, signer, account_id_hex)
}

async fn seed_canonical(state: &crate::state::AppState, account_id: &str, nonce: u64) {
    let delta = DeltaObject {
        account_id: account_id.to_string(),
        nonce,
        prev_commitment: format!("0xprev{nonce:04}"),
        new_commitment: Some(format!("0xnew{nonce:04}")),
        delta_payload: serde_json::json!({}),
        ack_sig: String::new(),
        ack_pubkey: String::new(),
        ack_scheme: String::new(),
        status: DeltaStatus::Canonical {
            timestamp: format!("2026-08-01T12:00:{:02}Z", nonce % 60),
        },
        metadata: None,
    };
    state
        .storage
        .submit_delta(&delta)
        .await
        .expect("seed delta");
}

#[tokio::test]
async fn test_grpc_history_paginates_newest_first() {
    let (state, service, signer, account_id_hex) = configured_service().await;
    for nonce in 1..=3 {
        seed_canonical(&state, &account_id_hex, nonce).await;
    }

    // Page 1 (limit 2): nonces 3, 2 and a resume cursor.
    let request = GetHistoryRequest {
        account_id: account_id_hex.clone(),
        limit: Some(2),
        cursor: None,
    };
    let response = service
        .get_history(create_signed_request_with_auth(
            request,
            &account_id_hex,
            &signer,
        ))
        .await
        .expect("history page 1")
        .into_inner();
    assert!(response.success, "{}", response.message);
    assert_eq!(
        response.entries.iter().map(|e| e.nonce).collect::<Vec<_>>(),
        vec![3, 2]
    );
    assert_eq!(response.entries[0].timestamp, "2026-08-01T12:00:03Z");
    assert_eq!(
        response.entries[0].new_commitment.as_deref(),
        Some("0xnew0003")
    );
    // Stub payloads do not decode: sections empty plus a warning.
    assert!(response.entries[0].input_notes.is_empty());
    assert_eq!(response.entries[0].decode_warnings[0].section, "tx_summary");
    let cursor = response.next_cursor.expect("page 1 cursor");

    // Page 2: nonce 1 only, end of feed.
    let request = GetHistoryRequest {
        account_id: account_id_hex.clone(),
        limit: Some(2),
        cursor: Some(cursor),
    };
    let response = service
        .get_history(create_signed_request_with_auth(
            request,
            &account_id_hex,
            &signer,
        ))
        .await
        .expect("history page 2")
        .into_inner();
    assert!(response.success);
    assert_eq!(
        response.entries.iter().map(|e| e.nonce).collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(response.next_cursor, None);
}

#[tokio::test]
async fn test_grpc_history_rejects_invalid_limit() {
    let (_state, service, signer, account_id_hex) = configured_service().await;

    let request = GetHistoryRequest {
        account_id: account_id_hex.clone(),
        limit: Some(501),
        cursor: None,
    };
    let status = service
        .get_history(create_signed_request_with_auth(
            request,
            &account_id_hex,
            &signer,
        ))
        .await
        .expect_err("limit 501 must be rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn test_grpc_history_rejects_unauthorized_cosigner() {
    let (_state, service, _signer, account_id_hex) = configured_service().await;

    let intruder = TestSigner::new();
    let request = GetHistoryRequest {
        account_id: account_id_hex.clone(),
        limit: None,
        cursor: None,
    };
    let status = service
        .get_history(create_signed_request_with_auth(
            request,
            &account_id_hex,
            &intruder,
        ))
        .await
        .expect_err("foreign cosigner must be rejected");
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
}
