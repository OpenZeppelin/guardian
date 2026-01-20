use crate::metadata::auth::{Auth, Credentials};
use crate::services::{ConfigureAccountParams, configure_account};
use crate::testing::fixtures;
use crate::testing::helpers::{create_test_app_state, generate_falcon_signature};

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

    let (pubkey_hex, commitment_hex, signature_hex) = generate_falcon_signature(&account_id);

    let params = ConfigureAccountParams {
        account_id: account_id.clone(),
        auth: Auth::MidenFalconRpo {
            cosigner_commitments: vec![commitment_hex.clone()],
        },
        initial_state: account_json.clone(),
        credential: Credentials::signature(pubkey_hex, signature_hex),
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
