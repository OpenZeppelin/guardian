//! gRPC integration tests for `GetStateTail` (issue #517).

use crate::api::grpc::guardian::guardian_server::Guardian;
use crate::api::grpc::guardian::{ConfigureRequest, GetStateRequest, GetStateTailRequest};
use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::state::AppState;
use crate::testing::fixtures;
use crate::testing::helpers::{
    IntegrationMockNetworkClient, TestSigner, create_grpc_service, create_miden_falcon_rpo_auth,
    create_miden_network_config, create_signed_request_with_auth, create_test_app_state,
    fixture_signer, load_fixture_account_grpc as load_fixture_account, load_queue_fixture_delta,
};
use std::sync::Arc;

async fn configured_service() -> (
    crate::api::grpc::GuardianService,
    AppState,
    TestSigner,
    String,
) {
    let mut state = create_test_app_state().await;
    state.network_client = Arc::new(IntegrationMockNetworkClient::new(
        MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal),
    ));
    let service = create_grpc_service(state.clone());

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let (signer, cosigner_commitments) = fixture_signer();
    let response = service
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
        .expect("configure should succeed");
    assert!(response.into_inner().success);

    (service, state, signer, account_id_hex)
}

fn tail_request(account_id: &str, signer: &TestSigner) -> tonic::Request<GetStateTailRequest> {
    create_signed_request_with_auth(
        GetStateTailRequest {
            account_id: account_id.to_string(),
        },
        account_id,
        signer,
    )
}

#[tokio::test]
async fn test_grpc_state_tail_is_the_canonical_state_with_nothing_queued() {
    let (service, _state, signer, account_id_hex) = configured_service().await;

    let tail = service
        .get_state_tail(tail_request(&account_id_hex, &signer))
        .await
        .expect("get_state_tail should succeed")
        .into_inner();
    assert!(tail.success);
    assert!(tail.error_code.is_empty());
    assert!(tail.queue.is_empty());

    let canonical = service
        .get_state(create_signed_request_with_auth(
            GetStateRequest {
                account_id: account_id_hex.clone(),
            },
            &account_id_hex,
            &signer,
        ))
        .await
        .expect("get_state should succeed")
        .into_inner()
        .state
        .expect("state present");
    assert_eq!(tail.state.expect("tail present"), canonical);
}

#[tokio::test]
async fn test_grpc_state_tail_replays_a_queued_candidate() {
    let (service, state, signer, account_id_hex) = configured_service().await;
    let canonical_commitment = state
        .storage
        .pull_state(&account_id_hex)
        .await
        .expect("configured")
        .commitment;
    let fixture = load_queue_fixture_delta(1);
    let commitments: serde_json::Value =
        serde_json::from_str(fixtures::COMMITMENTS_JSON).expect("commitments parse");
    let tail_commitment = commitments["commitment_after_queue_1"]
        .as_str()
        .expect("pinned commitment")
        .to_string();
    state
        .storage
        .submit_delta(&DeltaObject {
            account_id: account_id_hex.clone(),
            nonce: 1,
            prev_commitment: canonical_commitment.clone(),
            new_commitment: Some(tail_commitment.clone()),
            delta_payload: fixture["delta_payload"].clone(),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::candidate("2026-11-11T10:00:00Z".to_string()),
            metadata: None,
        })
        .await
        .expect("candidate stored");

    let tail = service
        .get_state_tail(tail_request(&account_id_hex, &signer))
        .await
        .expect("get_state_tail should succeed")
        .into_inner();
    let tail_state = tail.state.expect("tail present");
    assert_eq!(tail_state.commitment, tail_commitment);
    assert_eq!(tail_state.updated_at, "2026-11-11T10:00:00Z");
    assert_eq!(tail.queue.len(), 1);
    assert_eq!(tail.queue[0].nonce, 1);
    assert_eq!(tail.queue[0].prev_commitment, canonical_commitment);
    assert_eq!(tail.queue[0].new_commitment, tail_commitment);
}

#[tokio::test]
async fn test_grpc_state_tail_unknown_account_is_not_found() {
    let (service, _state, signer, _account_id_hex) = configured_service().await;

    let unknown = "0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c";
    let status = service
        .get_state_tail(tail_request(unknown, &signer))
        .await
        .expect_err("unknown account should fail");
    assert_eq!(status.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn test_grpc_state_tail_refuses_a_foreign_signer() {
    let (service, _state, _signer, account_id_hex) = configured_service().await;

    let status = service
        .get_state_tail(tail_request(&account_id_hex, &TestSigner::new()))
        .await
        .expect_err("a signer outside the account is refused");
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
}
