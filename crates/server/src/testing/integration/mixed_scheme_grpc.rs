//! Mixed Falcon + ECDSA accounts over gRPC (issue #563): `AuthConfig.miden`
//! on `Configure`, and each cosigner authenticating under its own scheme.

use crate::api::grpc::guardian::guardian_server::Guardian;
use crate::api::grpc::guardian::{
    AuthConfig, ConfigureRequest, GetStateRequest, MidenAuth, MidenCosigner, auth_config,
};
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::testing::helpers::{
    TestEcdsaSigner, TestSigner, build_guarded_multisig_account, create_grpc_service,
    create_miden_network_config, create_request_with_auth, create_signed_request_with_auth,
    create_test_app_state, word_from_commitment_hex,
};
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::{SignatureScheme, ToJson};
use std::sync::Arc;

fn miden_auth(cosigners: &[(&str, &str)], guardian_scheme: &str) -> AuthConfig {
    AuthConfig {
        auth_type: Some(auth_config::AuthType::Miden(MidenAuth {
            cosigners: cosigners
                .iter()
                .map(|(commitment, scheme)| MidenCosigner {
                    commitment: commitment.to_string(),
                    scheme: scheme.to_string(),
                })
                .collect(),
            guardian_scheme: guardian_scheme.to_string(),
        })),
    }
}

#[tokio::test]
async fn grpc_configures_a_mixed_account_and_each_cosigner_authenticates() {
    let mut state = create_test_app_state().await;
    state.network_client = Arc::new(MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal));
    let ecdsa_ack_commitment = state.ack.commitment(&SignatureScheme::Ecdsa);
    let service = create_grpc_service(state);

    let falcon = TestSigner::new();
    let ecdsa = TestEcdsaSigner::new();
    let account = build_guarded_multisig_account(
        [19u8; 32],
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
            word_from_commitment_hex(&ecdsa_ack_commitment),
            SignatureScheme::Ecdsa,
        ),
    );
    let account_id_hex = account.id().to_hex();
    let initial_state = serde_json::to_string(&account.to_json()).unwrap();
    let cosigners = [
        (falcon.commitment_hex.as_str(), "falcon"),
        (ecdsa.commitment_hex.as_str(), "ecdsa"),
    ];

    // An unknown scheme name is refused before anything is checked.
    let malformed = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(miden_auth(
            &[(falcon.commitment_hex.as_str(), "rsa")],
            "ecdsa",
        )),
        network_config: Some(create_miden_network_config()),
        initial_state: initial_state.clone(),
    };
    let status = service
        .configure(create_signed_request_with_auth(
            malformed,
            &account_id_hex,
            &falcon,
        ))
        .await
        .expect_err("an unknown scheme is malformed");
    assert_eq!(status.code(), tonic::Code::InvalidArgument, "{status:?}");

    let configure = ConfigureRequest {
        account_id: account_id_hex.clone(),
        auth: Some(miden_auth(&cosigners, "ecdsa")),
        network_config: Some(create_miden_network_config()),
        initial_state,
    };
    let response = service
        .configure(create_signed_request_with_auth(
            configure,
            &account_id_hex,
            &falcon,
        ))
        .await
        .expect("the mixed account configures over gRPC")
        .into_inner();
    assert!(response.success, "{}", response.message);
    assert_eq!(response.ack_commitment, ecdsa_ack_commitment);

    let get_state = || GetStateRequest {
        account_id: account_id_hex.clone(),
    };
    let falcon_read = service
        .get_state(create_signed_request_with_auth(
            get_state(),
            &account_id_hex,
            &falcon,
        ))
        .await
        .expect("the Falcon cosigner reads the state")
        .into_inner();
    assert_eq!(
        falcon_read.state.expect("state").auth_scheme.as_deref(),
        Some("mixed")
    );

    let payload = AuthRequestPayload::from_protobuf_message(&get_state());
    let (signature, timestamp) = ecdsa.sign_request(&account_id_hex, &payload);
    service
        .get_state(create_request_with_auth(
            get_state(),
            &ecdsa.pubkey_hex,
            &signature,
            timestamp,
        ))
        .await
        .expect("the ECDSA cosigner reads the state");

    let stranger = TestEcdsaSigner::new();
    let (signature, timestamp) = stranger.sign_request(&account_id_hex, &payload);
    let status = service
        .get_state(create_request_with_auth(
            get_state(),
            &stranger.pubkey_hex,
            &signature,
            timestamp,
        ))
        .await
        .expect_err("a key that is not a cosigner is refused");
    assert_eq!(status.code(), tonic::Code::Unauthenticated, "{status:?}");
}
