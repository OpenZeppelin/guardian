//! gRPC integration tests for `GetCanonicalNonce` (issue #191).

use crate::api::grpc::guardian::guardian_server::Guardian;
use crate::api::grpc::guardian::{ConfigureRequest, GetCanonicalNonceRequest, GetStateRequest};
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::testing::helpers::{
    IntegrationMockNetworkClient, TestSigner, create_grpc_service, create_miden_falcon_rpo_auth,
    create_miden_network_config, create_signed_request_with_auth, create_test_app_state,
    fixture_signer, load_fixture_account_grpc as load_fixture_account,
};
use std::sync::Arc;

/// The fixture account configured through the real Miden decoder, so the
/// nonce the endpoint serves is read from the stored account state itself
/// rather than from a mock.
async fn configured_service() -> (crate::api::grpc::GuardianService, TestSigner, String) {
    let mut state = create_test_app_state().await;
    state.network_client = Arc::new(IntegrationMockNetworkClient::new(
        MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal),
    ));
    let service = create_grpc_service(state);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let (signer, cosigner_commitments) = fixture_signer();

    let configure_req = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(create_miden_falcon_rpo_auth(cosigner_commitments)),
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

    (service, signer, account_id_hex)
}

#[tokio::test]
async fn test_grpc_canonical_nonce_matches_get_state() {
    let (service, signer, account_id_hex) = configured_service().await;

    let nonce = service
        .get_canonical_nonce(create_signed_request_with_auth(
            GetCanonicalNonceRequest {
                account_id: account_id_hex.clone(),
            },
            &account_id_hex,
            &signer,
        ))
        .await
        .expect("get_canonical_nonce should succeed")
        .into_inner();
    assert!(nonce.success);
    assert_eq!(nonce.account_id, account_id_hex);
    assert!(nonce.error_code.is_empty());

    let state = service
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
    assert_eq!(nonce.commitment, state.commitment);
}

#[tokio::test]
async fn test_grpc_canonical_nonce_unknown_account_is_not_found() {
    let (service, signer, _account_id_hex) = configured_service().await;

    let unknown = "0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c".to_string();
    let status = service
        .get_canonical_nonce(create_signed_request_with_auth(
            GetCanonicalNonceRequest {
                account_id: unknown.clone(),
            },
            &unknown,
            &signer,
        ))
        .await
        .expect_err("unknown account should fail");
    assert_eq!(status.code(), tonic::Code::NotFound);
}
