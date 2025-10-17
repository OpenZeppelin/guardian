use crate::network::NetworkType;
use miden_objects::account::AccountId;
use miden_rpc_client::MidenRpcClient;


/// Miden network client for fetching on-chain account data
pub struct MidenNetworkClient {
    client: MidenRpcClient,
}

impl MidenNetworkClient {
    /// Create a new Miden network client from a NetworkType
    pub async fn from_network(network: NetworkType) -> Result<Self, String> {
        let endpoint = network.rpc_endpoint();
        let client = MidenRpcClient::connect(endpoint).await?;
        Ok(Self { client })
    }

    /// Verify that the initial state is valid for the account.
    ///
    /// # Arguments
    /// * `account_id_hex` - Account ID as hex string
    /// * `state_json` - The initial state JSON
    ///
    /// # Returns
    /// * `Ok(commitment)` - The on-chain commitment hash
    /// * `Err(String)` - Account doesn't exist, network error, or validation failed
    pub async fn verify_intial_state(
        &mut self,
        account_id_hex: &str,
        _state_json: &serde_json::Value,
    ) -> Result<String, String> {
        // Parse and validate account ID format
        let account_id = AccountId::from_hex(account_id_hex)
            .map_err(|e| format!("Invalid Miden account ID format: {}", e))?;

        // Fetch on-chain commitment - this verifies the account exists
        let commitment = self.client
            .get_account_commitment(&account_id)
            .await
            .map_err(|e| {
                format!(
                    "Failed to verify account '{}' on Miden network: {}",
                    account_id_hex, e
                )
            })?;

        // TODO: In the future, we could validate that state_json is consistent
        // with the on-chain state by computing a local commitment and comparing

        Ok(commitment)
    }

    /// Fetch account commitment from the Miden network
    /// Returns the commitment hash as a hex string
    pub async fn get_account_commitment(
        &mut self,
        account_id: &AccountId,
    ) -> Result<String, String> {
        self.client.get_account_commitment(account_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_type_rpc_endpoint() {
        let network = NetworkType::MidenTestnet;
        assert_eq!(network.rpc_endpoint(), "https://rpc.testnet.miden.io");
    }

    #[tokio::test]
    async fn test_client_from_network_type() {
        let network = NetworkType::MidenTestnet;
        let result = MidenNetworkClient::from_network(network).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_verify_account() {
        let network = NetworkType::MidenTestnet;
        let mut client = MidenNetworkClient::from_network(network)
            .await
            .expect("Failed to create client");

        // Test with a real account that exists on testnet
        let account_id_hex = "0x8a65fc5a39e4cd106d648e3eb4ab5f";
        let state_json = serde_json::json!({"balance": 0});

        let result = client.verify_intial_state(account_id_hex, &state_json).await;
        assert!(result.is_ok(), "Verify account should succeed: {:?}", result.err());

        let commitment = result.unwrap();
        assert!(commitment.starts_with("0x"), "Commitment should be hex string");
        assert_eq!(commitment.len(), 66, "Commitment should be 32 bytes (66 hex chars with 0x)");
    }

    #[tokio::test]
    async fn test_verify_account_invalid_format() {
        let network = NetworkType::MidenTestnet;
        let mut client = MidenNetworkClient::from_network(network)
            .await
            .expect("Failed to create client");

        // Test with invalid account ID format
        let invalid_account_id = "not_a_valid_hex";
        let state_json = serde_json::json!({"balance": 0});

        let result = client.verify_intial_state(invalid_account_id, &state_json).await;
        assert!(result.is_err(), "Should fail with invalid account ID");
        assert!(result.unwrap_err().contains("Invalid Miden account ID format"));
    }

}
