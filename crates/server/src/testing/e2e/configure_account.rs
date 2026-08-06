use crate::error::GuardianError;
use crate::metadata::auth::{Auth, Credentials};
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::services::{ConfigureAccountParams, configure_account};
use crate::testing::fixtures;
use crate::testing::helpers::{
    IntegrationMockNetworkClient, create_test_app_state, generate_falcon_signature,
};
use std::sync::Arc;

#[tokio::test]
async fn test_configure_account_with_real_miden_account() {
    let state = create_test_app_state().await;

    let account_json: serde_json::Value =
        serde_json::from_str(fixtures::ACCOUNT_JSON).expect("Failed to parse account.json");
    let commitments_json: serde_json::Value =
        serde_json::from_str(fixtures::COMMITMENTS_JSON).expect("Failed to parse commitments.json");

    let account_id = commitments_json["account_id"]
        .as_str()
        .expect("Missing account_id")
        .to_string();

    let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
        generate_falcon_signature(&account_id);

    let params = ConfigureAccountParams {
        account_id: account_id.clone(),
        auth: Auth::MidenFalconRpo {
            cosigner_commitments: vec![commitment_hex.clone()],
        },
        network_config: crate::metadata::NetworkConfig::miden_default(),
        initial_state: account_json.clone(),
        credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
    };

    let result = configure_account(&state, params).await;

    assert!(
        result.is_ok(),
        "configure_account failed: {:?}",
        result.err()
    );

    let metadata_entry = state
        .metadata
        .get(&account_id)
        .await
        .expect("Failed to get metadata")
        .expect("Metadata not found");

    assert_eq!(metadata_entry.account_id, account_id);
    assert_eq!(
        metadata_entry.auth,
        Auth::MidenFalconRpo {
            cosigner_commitments: vec![commitment_hex],
        }
    );
}

/// #102: with a real (non-mock) extraction path, /configure must reject a
/// declared cosigner set that is not the signer set stored in the submitted
/// account state — here a freshly generated key that is not a signer of the
/// fixture account.
#[tokio::test]
async fn test_configure_account_rejects_commitments_not_in_real_account_state() {
    let mut state = create_test_app_state().await;
    state.network_client = Arc::new(IntegrationMockNetworkClient::new(
        MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal),
    ));

    let account_json: serde_json::Value =
        serde_json::from_str(fixtures::ACCOUNT_JSON).expect("Failed to parse account.json");
    let commitments_json: serde_json::Value =
        serde_json::from_str(fixtures::COMMITMENTS_JSON).expect("Failed to parse commitments.json");

    let account_id = commitments_json["account_id"]
        .as_str()
        .expect("Missing account_id")
        .to_string();

    let (pubkey_hex, commitment_hex, signature_hex, timestamp) =
        generate_falcon_signature(&account_id);

    let params = ConfigureAccountParams {
        account_id: account_id.clone(),
        auth: Auth::MidenFalconRpo {
            cosigner_commitments: vec![commitment_hex],
        },
        network_config: crate::metadata::NetworkConfig::miden_default(),
        initial_state: account_json,
        credential: Credentials::signature(pubkey_hex, signature_hex, timestamp),
    };

    let err = configure_account(&state, params)
        .await
        .expect_err("configuring with a non-signer commitment must be rejected");
    assert!(
        matches!(err, GuardianError::InvalidInput(_)),
        "expected InvalidInput, got: {err:?}"
    );

    assert!(
        state
            .metadata
            .get(&account_id)
            .await
            .expect("metadata readable")
            .is_none(),
        "no metadata should be stored for a rejected configuration"
    );
}
