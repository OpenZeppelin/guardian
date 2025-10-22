use miden_lib::account::{auth::AuthRpoFalcon512Multisig, wallets::BasicWallet};
use miden_objects::{
    Felt, Word,
    account::delta::{AccountStorageDelta, AccountVaultDelta},
    account::{Account, AccountBuilder, AccountDelta},
    crypto::dsa::rpo_falcon512::PublicKey,
};
use private_state_manager_shared::{FromJson, ToJson};
use std::fs;

#[tokio::test]
#[ignore] // Run manually with: cargo test --test generate_fixtures generate_multisig_fixtures -- --ignored
async fn generate_multisig_fixtures() {
    let pub_key_1 = PublicKey::new(Word::from([1u32, 0, 0, 0]));
    let pub_key_2 = PublicKey::new(Word::from([2u32, 0, 0, 0]));
    let pub_key_3 = PublicKey::new(Word::from([3u32, 0, 0, 0]));
    let approvers = vec![pub_key_1, pub_key_2, pub_key_3];
    let threshold = 2u32;

    let multisig_component = AuthRpoFalcon512Multisig::new(threshold, approvers.clone())
        .expect("multisig component creation failed");

    let (account, _) = AccountBuilder::new([0xff; 32])
        .with_auth_component(multisig_component)
        .with_component(BasicWallet)
        .build()
        .expect("account building failed");

    let account_json = account.to_json();
    let account_id = account.id();
    let mut current_commitment = account.commitment();

    println!("\nGenerated Multisig Account:");
    println!("  Account ID: {account_id}");
    println!(
        "  Commitment: 0x{}",
        hex::encode(current_commitment.as_bytes())
    );
    println!("  Threshold: {}/{}", threshold, approvers.len());
    println!("  Approvers:");
    for (i, pub_key) in approvers.iter().enumerate() {
        println!("    {}: {}", i, Word::from(*pub_key));
    }

    let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("account.json");

    fs::write(
        &fixture_path,
        serde_json::to_string_pretty(&account_json).unwrap(),
    )
    .expect("Failed to write account.json");

    println!("✅ Multisig account fixture saved to account.json");

    let mut commitments = vec![(
        "initial_commitment".to_string(),
        format!("0x{}", hex::encode(current_commitment.as_bytes())),
    )];

    // Delta 1: Add 4th approver
    let pub_key_4 = PublicKey::new(Word::from([4u32, 0, 0, 0]));
    let mut storage_delta_1 = AccountStorageDelta::default();
    storage_delta_1.set_map_item(1, Word::from([3u32, 0, 0, 0]), Word::from(pub_key_4));
    storage_delta_1.set_item(0, Word::from([threshold, 4u32, 0, 0]));

    let delta_1 = AccountDelta::new(
        account_id,
        storage_delta_1,
        AccountVaultDelta::default(),
        Felt::new(1),
    )
    .expect("Failed to create delta 1");

    let mut account_state = Account::from_json(&account_json).expect("Failed to deserialize");
    let prev_commitment_1 = current_commitment;
    account_state
        .apply_delta(&delta_1)
        .expect("Failed to apply delta 1");
    current_commitment = account_state.commitment();

    println!("\nDelta 1 - Added 4th approver:");
    println!("  New approver: {}", Word::from(pub_key_4));
    println!(
        "  Commitment: 0x{}",
        hex::encode(current_commitment.as_bytes())
    );

    let delta_1_fixture = serde_json::json!({
        "account_id": format!("{}", account_id),
        "nonce": 1,
        "prev_commitment": format!("0x{}", hex::encode(prev_commitment_1.as_bytes())),
        "new_commitment": format!("0x{}", hex::encode(current_commitment.as_bytes())),
        "delta_payload": delta_1.to_json()
    });

    fs::write(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("delta_1.json"),
        serde_json::to_string_pretty(&delta_1_fixture).unwrap(),
    )
    .expect("Failed to write delta_1.json");

    commitments.push((
        "commitment_after_delta_1".to_string(),
        format!("0x{}", hex::encode(current_commitment.as_bytes())),
    ));

    // Delta 2: Add 5th approver
    let pub_key_5 = PublicKey::new(Word::from([5u32, 0, 0, 0]));
    let mut storage_delta_2 = AccountStorageDelta::default();
    storage_delta_2.set_map_item(1, Word::from([4u32, 0, 0, 0]), Word::from(pub_key_5));
    storage_delta_2.set_item(0, Word::from([threshold, 5u32, 0, 0]));

    let delta_2 = AccountDelta::new(
        account_id,
        storage_delta_2,
        AccountVaultDelta::default(),
        Felt::new(1),
    )
    .expect("Failed to create delta 2");

    let prev_commitment_2 = current_commitment;
    account_state
        .apply_delta(&delta_2)
        .expect("Failed to apply delta 2");
    current_commitment = account_state.commitment();

    println!("\nDelta 2 - Added 5th approver:");
    println!("  New approver: {}", Word::from(pub_key_5));
    println!(
        "  Commitment: 0x{}",
        hex::encode(current_commitment.as_bytes())
    );

    let delta_2_fixture = serde_json::json!({
        "account_id": format!("{}", account_id),
        "nonce": 2,
        "prev_commitment": format!("0x{}", hex::encode(prev_commitment_2.as_bytes())),
        "new_commitment": format!("0x{}", hex::encode(current_commitment.as_bytes())),
        "delta_payload": delta_2.to_json()
    });

    fs::write(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("delta_2.json"),
        serde_json::to_string_pretty(&delta_2_fixture).unwrap(),
    )
    .expect("Failed to write delta_2.json");

    commitments.push((
        "commitment_after_delta_2".to_string(),
        format!("0x{}", hex::encode(current_commitment.as_bytes())),
    ));

    // Delta 3: Increase threshold to 3
    let mut storage_delta_3 = AccountStorageDelta::default();
    storage_delta_3.set_item(0, Word::from([3u32, 5u32, 0, 0]));

    let delta_3 = AccountDelta::new(
        account_id,
        storage_delta_3,
        AccountVaultDelta::default(),
        Felt::new(1),
    )
    .expect("Failed to create delta 3");

    let prev_commitment_3 = current_commitment;
    account_state
        .apply_delta(&delta_3)
        .expect("Failed to apply delta 3");
    current_commitment = account_state.commitment();

    println!("\nDelta 3 - Increased threshold to 3:");
    println!("  New threshold: 3/5");
    println!(
        "  Commitment: 0x{}",
        hex::encode(current_commitment.as_bytes())
    );

    let delta_3_fixture = serde_json::json!({
        "account_id": format!("{}", account_id),
        "nonce": 3,
        "prev_commitment": format!("0x{}", hex::encode(prev_commitment_3.as_bytes())),
        "new_commitment": format!("0x{}", hex::encode(current_commitment.as_bytes())),
        "delta_payload": delta_3.to_json()
    });

    fs::write(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("delta_3.json"),
        serde_json::to_string_pretty(&delta_3_fixture).unwrap(),
    )
    .expect("Failed to write delta_3.json");

    commitments.push((
        "commitment_after_delta_3".to_string(),
        format!("0x{}", hex::encode(current_commitment.as_bytes())),
    ));

    // Save commitments summary
    let mut commitments_map = serde_json::Map::new();
    commitments_map.insert(
        "account_id".to_string(),
        serde_json::json!(format!("{}", account_id)),
    );
    for (key, value) in commitments {
        commitments_map.insert(key, serde_json::json!(value));
    }

    fs::write(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("commitments.json"),
        serde_json::to_string_pretty(&commitments_map).unwrap(),
    )
    .expect("Failed to write commitments.json");

    println!("\n✅ Saved commitments.json");
    println!("\n✅ All multisig fixtures generated successfully!");
    println!("\nFixtures created:");
    println!("  - account.json (initial state: 2/3 multisig)");
    println!("  - delta_1.json (add 4th approver)");
    println!("  - delta_2.json (add 5th approver)");
    println!("  - delta_3.json (increase threshold to 3)");
    println!("  - commitments.json (all commitments)");
}
