use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;

/// Integration test for verifying we can connect to Miden devnet.
///
/// Run this ignored test with `GUARDIAN_NETWORK_TESTS=1`.
#[tokio::test]
#[ignore = "requires live Miden devnet access; run with GUARDIAN_NETWORK_TESTS=1"]
async fn test_fetch_account_commitment_from_devnet() {
    if std::env::var("GUARDIAN_NETWORK_TESTS").as_deref() != Ok("1") {
        return;
    }

    let _client = MidenNetworkClient::from_network(NetworkType::MidenDevnet)
        .await
        .expect("Failed to create Miden network client");

    // Also perform a direct RPC call to assert connectivity
    let endpoint = NetworkType::MidenDevnet.rpc_endpoint();
    let mut rpc_client = miden_rpc_client::MidenRpcClient::connect(endpoint)
        .await
        .expect("Failed to connect RPC client");
    rpc_client
        .get_status()
        .await
        .expect("Status RPC call failed");
}
