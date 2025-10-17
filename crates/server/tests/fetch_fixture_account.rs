//! Script to fetch account data from Miden testnet and save as fixture
//!
//! Run with: cargo test --package private-state-manager-server --test fetch_fixture_account -- --nocapture

use base64::Engine;
use miden_objects::account::AccountId;
use miden_objects::utils::Deserializable;
use miden_rpc_client::MidenRpcClient;
use std::fs;
use std::path::PathBuf;

#[tokio::test]
async fn fetch_and_save_fixture_account() {
    // Account ID to fetch
    let account_id_hex = "0x14cc48b68d9e370034de98f8ca1788";
    let expected_commitment = "0xa76d2a39784ebaf674f05f4a2138149c3ebdc5bb738eb7fed7f40af295a0d973";

    println!("Fetching account {} from testnet...", account_id_hex);

    let account_id = AccountId::from_hex(account_id_hex).expect("Valid account ID");

    // Connect to testnet
    let mut client = MidenRpcClient::connect("https://rpc.testnet.miden.io")
        .await
        .expect("Failed to connect to testnet");

    // Fetch account details
    let account_details = client
        .get_account_details(&account_id)
        .await
        .expect("Failed to fetch account details");

    // Verify we got the summary
    let summary = account_details
        .summary
        .expect("No account summary in response");

    // Verify commitment matches expected
    let commitment = summary
        .account_commitment
        .expect("No commitment in account summary");

    let commitment_bytes = [
        commitment.d0.to_le_bytes(),
        commitment.d1.to_le_bytes(),
        commitment.d2.to_le_bytes(),
        commitment.d3.to_le_bytes(),
    ]
    .concat();

    let commitment_hex = format!("0x{}", hex::encode(&commitment_bytes));
    assert_eq!(
        commitment_hex, expected_commitment,
        "Commitment mismatch! Expected {}, got {}",
        expected_commitment, commitment_hex
    );

    println!("✓ Commitment verified: {}", commitment_hex);

    // Get the account bytes
    let account_bytes = account_details
        .details
        .expect("No account details bytes in response");

    println!("✓ Account bytes length: {} bytes", account_bytes.len());

    // Verify we can deserialize the account
    let account = miden_objects::account::Account::read_from_bytes(&account_bytes)
        .expect("Failed to deserialize account");

    println!("✓ Account ID from bytes: {}", account.id().to_hex());
    println!("✓ Account nonce: {}", account.nonce());

    // Encode to base64 for JSON
    let base64_data = base64::engine::general_purpose::STANDARD.encode(&account_bytes);

    // Create JSON in the format expected by FromJson trait
    let account_json = serde_json::json!({
        "data": base64_data,
        "account_id": account_id_hex,
    });

    // Create fixtures directory
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");

    fs::create_dir_all(&fixtures_dir).expect("Failed to create fixtures directory");

    // Save to file
    let fixture_path = fixtures_dir.join("account.json");
    let json_str = serde_json::to_string_pretty(&account_json).expect("Failed to serialize JSON");
    fs::write(&fixture_path, json_str).expect("Failed to write fixture file");

    println!("✓ Fixture saved to: {}", fixture_path.display());
    println!("\nFixture contents:");
    println!("  account_id: {}", account_id_hex);
    println!("  commitment: {}", commitment_hex);
    println!("  data_size: {} bytes (base64)", base64_data.len());
}
