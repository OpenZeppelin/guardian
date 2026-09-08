use crate::error::GuardianError;
use crate::metadata::auth::{Auth, Credentials};
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::services::{ConfigureAccountParams, configure_account};
use crate::testing::fixtures;
use crate::testing::helpers::{
    IntegrationMockNetworkClient, create_test_app_state, generate_falcon_signature,
};
use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::hex::IntoHex;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use std::sync::Arc;

/// The fixture signer keys and the account's full cosigner set, in the
/// signer map's canonical (index) order, as generated alongside
/// `account.json` by `generate_fixtures`.
fn fixture_signer() -> (SecretKey, String, Vec<String>) {
    let keys: serde_json::Value =
        serde_json::from_str(fixtures::KEYS_JSON).expect("Failed to parse keys.json");
    let secret_key_bytes = hex::decode(keys["signer_1_secret_key"].as_str().expect("signer key"))
        .expect("signer key hex");
    let secret_key = SecretKey::read_from_bytes(&secret_key_bytes).expect("signer key bytes");
    let pubkey_hex = secret_key.public_key().into_hex();
    let cosigner_commitments = (1..=3)
        .map(|i| {
            keys[format!("signer_{i}_commitment")]
                .as_str()
                .expect("signer commitment")
                .to_string()
        })
        .collect();
    (secret_key, pubkey_hex, cosigner_commitments)
}

fn falcon_credentials(
    key: &SecretKey,
    pubkey_hex: &str,
    account_id_hex: &str,
    timestamp: i64,
) -> Credentials {
    let message = AuthRequestMessage::from_account_id_hex(
        account_id_hex,
        timestamp,
        AuthRequestPayload::empty(),
    )
    .expect("valid account ID")
    .to_word();
    let signature = key.sign(message);
    let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));
    Credentials::signature(pubkey_hex.to_string(), signature_hex, timestamp)
}

/// Configures the real fixture account through the real extraction path:
/// the declared cosigner set is the account's actual 3-signer map (from
/// `keys.json`, in map order) and the credential is signed by fixture
/// signer 1. This only passes because the declared list matches the state
/// — the companion test below proves a non-matching list is rejected.
#[tokio::test]
async fn test_configure_account_with_real_miden_account() {
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

    let (secret_key, pubkey_hex, cosigner_commitments) = fixture_signer();
    let timestamp = chrono::Utc::now().timestamp_millis();

    let params = ConfigureAccountParams {
        account_id: account_id.clone(),
        auth: Auth::MidenFalconRpo {
            cosigner_commitments: cosigner_commitments.clone(),
        },
        network_config: crate::metadata::NetworkConfig::miden_default(),
        initial_state: account_json.clone(),
        credential: falcon_credentials(&secret_key, &pubkey_hex, &account_id, timestamp),
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
            cosigner_commitments,
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
