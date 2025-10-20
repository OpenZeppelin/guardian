use crate::network::NetworkType;
use miden_objects::account::{Account, AccountId};
use miden_rpc_client::MidenRpcClient;
use private_state_manager_shared::FromJson;

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
    /// * `state_json` - The initial state JSON with "data" field containing base64-encoded account bytes
    ///
    /// # Returns
    /// * `Ok(commitment)` - The on-chain commitment hash (after full validation)
    pub async fn verify_intial_state(
        &mut self,
        account_id_hex: &str,
        state_json: &serde_json::Value,
    ) -> Result<String, String> {
        // Parse and validate account ID format
        let account_id = AccountId::from_hex(account_id_hex)
            .map_err(|e| format!("Invalid Miden account ID format: {e}"))?;

        // Fetch on-chain commitment - this verifies the account exists
        let on_chain_commitment = self
            .client
            .get_account_commitment(&account_id)
            .await
            .map_err(|e| {
                format!("Failed to verify account '{account_id_hex}' on Miden network: {e}")
            })?;

        // Construct account from state_json and validate commitment
        // This will return an error if the JSON format is invalid
        let account = Self::construct_account_from_json(&account_id, state_json)?;

        let local_commitment = account.commitment();
        let local_commitment_hex = format!("0x{}", hex::encode(local_commitment.as_bytes()));

        if local_commitment_hex != on_chain_commitment {
            return Err(format!(
                "Commitment mismatch for account '{account_id_hex}': local={local_commitment_hex}, on-chain={on_chain_commitment}"
            ));
        }

        Ok(on_chain_commitment)
    }

    /// Fetch account commitment from the Miden network
    /// Returns the commitment hash as a hex string
    pub async fn get_account_commitment(
        &mut self,
        account_id: &AccountId,
    ) -> Result<String, String> {
        self.client.get_account_commitment(account_id).await
    }

    /// Construct an Account object from JSON state representation
    ///
    /// # Arguments
    /// * `account_id` - The expected account ID
    /// * `state_json` - JSON representation with "data" field containing base64-encoded account bytes
    fn construct_account_from_json(
        account_id: &AccountId,
        state_json: &serde_json::Value,
    ) -> Result<Account, String> {
        let account = Account::from_json(state_json)?;

        if &account.id() != account_id {
            return Err(format!(
                "Account ID mismatch: expected {}, got {}",
                account_id.to_hex(),
                account.id().to_hex()
            ));
        }

        Ok(account)
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
    async fn test_verify_account_invalid_state_json() {
        let network = NetworkType::MidenTestnet;
        let mut client = MidenNetworkClient::from_network(network)
            .await
            .expect("Failed to create client");

        // Test with a real account that exists on testnet but invalid state JSON
        // This should fail because state JSON is missing the required "data" field
        let account_id_hex = "0x8a65fc5a39e4cd106d648e3eb4ab5f";
        let state_json = serde_json::json!({"balance": 0});

        let result = client
            .verify_intial_state(account_id_hex, &state_json)
            .await;
        assert!(
            result.is_err(),
            "Should fail with invalid state JSON format"
        );
        assert!(
            result.unwrap_err().contains("data"),
            "Error should mention missing 'data' field"
        );
    }

    #[tokio::test]
    async fn test_verify_account_with_fixture_data() {
        let network = NetworkType::MidenTestnet;
        let mut client = MidenNetworkClient::from_network(network)
            .await
            .expect("Failed to create client");

        // Load fixture account with real data from fixtures/account.json
        let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("server")
            .join("tests")
            .join("fixtures")
            .join("account.json");

        let fixture_contents = match std::fs::read_to_string(&fixture_path) {
            Ok(contents) => contents,
            Err(_) => {
                println!(
                    "⚠️  Fixture not found - skipping test. Run fetch_fixture_account test first."
                );
                return;
            }
        };

        let state_json: serde_json::Value =
            serde_json::from_str(&fixture_contents).expect("Failed to parse fixture JSON");

        let account_id_hex = state_json["account_id"]
            .as_str()
            .expect("No account_id in fixture");

        let expected_commitment =
            "0xa76d2a39784ebaf674f05f4a2138149c3ebdc5bb738eb7fed7f40af295a0d973";

        println!("Testing with fixture account: {}", account_id_hex);
        println!("Expected commitment: {}", expected_commitment);

        // This should succeed with full commitment validation
        let result = client
            .verify_intial_state(account_id_hex, &state_json)
            .await;

        assert!(
            result.is_ok(),
            "Should succeed with valid fixture data: {:?}",
            result.err()
        );

        let commitment = result.unwrap();
        assert_eq!(
            commitment, expected_commitment,
            "Commitment should match expected value"
        );

        println!("✓ Full commitment validation passed!");
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

        let result = client
            .verify_intial_state(invalid_account_id, &state_json)
            .await;
        assert!(result.is_err(), "Should fail with invalid account ID");
        assert!(
            result
                .unwrap_err()
                .contains("Invalid Miden account ID format")
        );
    }
}
