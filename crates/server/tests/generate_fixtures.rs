use miden_objects::{
    account::{Account, AccountDelta, AccountId, delta::{AccountStorageDelta, AccountVaultDelta}},
    Felt,
};
use private_state_manager_shared::ToJson;
use std::fs;

#[tokio::test]
#[ignore] // Run manually with: cargo test --test generate_fixtures -- --ignored
async fn generate_account_and_delta_fixtures() {
    // Load existing fixture account
    let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("account.json");

    let fixture_contents = fs::read_to_string(&fixture_path).expect("Failed to read account fixture");
    let account_json: serde_json::Value = serde_json::from_str(&fixture_contents).unwrap();

    let account_id_hex = account_json["account_id"].as_str().unwrap();
    let account_id = AccountId::from_hex(account_id_hex).unwrap();

    // Deserialize the account
    use private_state_manager_shared::FromJson;
    let account = Account::from_json(&account_json).expect("Failed to deserialize account");
    let initial_commitment = format!("0x{}", hex::encode(account.commitment().as_bytes()));

    println!("Initial account:");
    println!("  ID: {}", account_id_hex);
    println!("  Commitment: {}", initial_commitment);

    // Create first delta - increment nonce by 1
    let delta_1 = AccountDelta::new(
        account_id.clone(),
        AccountStorageDelta::default(),
        AccountVaultDelta::default(),
        Felt::new(1), // nonce delta: increment by 1
    ).expect("Failed to create delta 1");

    // Apply first delta
    let mut account_after_delta_1 = account.clone();
    account_after_delta_1
        .apply_delta(&delta_1)
        .expect("Failed to apply delta 1");
    let commitment_after_delta_1 = format!("0x{}", hex::encode(account_after_delta_1.commitment().as_bytes()));

    println!("\nAfter delta 1:");
    println!("  Commitment: {}", commitment_after_delta_1);

    // Save delta 1 fixture
    let delta_1_fixture = serde_json::json!({
        "account_id": account_id_hex,
        "nonce": 1,
        "prev_commitment": initial_commitment,
        "new_commitment": commitment_after_delta_1,
        "delta_payload": delta_1.to_json()
    });

    let delta_1_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("delta_1.json");

    fs::write(
        &delta_1_path,
        serde_json::to_string_pretty(&delta_1_fixture).unwrap(),
    )
    .expect("Failed to write delta_1.json");

    println!("✓ Saved delta_1.json");

    // Create second delta - increment nonce by 1 again
    let delta_2 = AccountDelta::new(
        account_id.clone(),
        AccountStorageDelta::default(),
        AccountVaultDelta::default(),
        Felt::new(1), // nonce delta: increment by 1
    ).expect("Failed to create delta 2");

    // Apply second delta
    let mut account_after_delta_2 = account_after_delta_1.clone();
    account_after_delta_2
        .apply_delta(&delta_2)
        .expect("Failed to apply delta 2");
    let commitment_after_delta_2 = format!("0x{}", hex::encode(account_after_delta_2.commitment().as_bytes()));

    println!("\nAfter delta 2:");
    println!("  Commitment: {}", commitment_after_delta_2);

    // Save delta 2 fixture
    let delta_2_fixture = serde_json::json!({
        "account_id": account_id_hex,
        "nonce": 2,
        "prev_commitment": commitment_after_delta_1,
        "new_commitment": commitment_after_delta_2,
        "delta_payload": delta_2.to_json()
    });

    let delta_2_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("delta_2.json");

    fs::write(
        &delta_2_path,
        serde_json::to_string_pretty(&delta_2_fixture).unwrap(),
    )
    .expect("Failed to write delta_2.json");

    println!("✓ Saved delta_2.json");

    // Update the summary fixture with commitments
    let summary_fixture = serde_json::json!({
        "account_id": account_id_hex,
        "initial_commitment": initial_commitment,
        "commitment_after_delta_1": commitment_after_delta_1,
        "commitment_after_delta_2": commitment_after_delta_2
    });

    let summary_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("commitments.json");

    fs::write(
        &summary_path,
        serde_json::to_string_pretty(&summary_fixture).unwrap(),
    )
    .expect("Failed to write commitments.json");

    println!("✓ Saved commitments.json");
    println!("\n✅ All fixtures generated successfully!");
}
