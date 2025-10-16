use miden_objects::account::AccountId;
use server::network::miden::MidenNetworkClient;
use server::network::NetworkType;

/// Integration test for fetching account commitment from Miden testnet
/// To run: cargo test --package private-state-manager-server --test miden_rpc_integration_test
#[tokio::test]
async fn test_fetch_account_commitment_from_testnet() {
    let account_id_hex = "0x8a65fc5a39e4cd106d648e3eb4ab5f";
    let expected_commitment = "0x5a07d85bf51a422c9ae70ca259fc2891a879b27a22da05b6a6cd8f1349b82533";

    let account_id = AccountId::from_hex(account_id_hex)
        .expect("Failed to parse account ID");

    let client = MidenNetworkClient::from_network(NetworkType::MidenTestnet)
        .expect("Failed to create Miden network client");

    let result = client.get_account_commitment(&account_id).await;

    assert!(result.is_ok(), "Failed to fetch account commitment: {:?}", result.err());

    let commitment = result.unwrap();

    assert!(commitment.starts_with("0x"), "Commitment should start with 0x");
    assert_eq!(commitment, expected_commitment, "Commitment mismatch");
}

