use crate::network::NetworkType;
use miden_client::rpc::{Endpoint, NodeRpcClient, TonicRpcClient, domain::account::FetchedAccount};
use miden_objects::account::AccountId;
use std::sync::Arc;

/// Miden network client for fetching on-chain account data
pub struct MidenNetworkClient {
    rpc_client: Arc<dyn NodeRpcClient>,
}

impl MidenNetworkClient {
    /// Create a new Miden network client
    pub fn from_network(network: NetworkType) -> Result<Self, String> {
        let endpoint = match network {
            NetworkType::MidenTestnet => Endpoint::testnet(),
        };

        let rpc_client = TonicRpcClient::new(&endpoint, 10000);

        Ok(Self {
            rpc_client: Arc::new(rpc_client),
        })
    }

    /// Get account details from the Miden network
    pub async fn get_account_details(
        &self,
        account_id: &AccountId,
    ) -> Result<FetchedAccount, String> {
        self.rpc_client
            .get_account_details(*account_id)
            .await
            .map_err(|e| format!("Failed to fetch account details: {}", e))
    }

    /// Fetch account commitment from the Miden network
    /// Returns the commitment hash as a hex string
    pub async fn get_account_commitment(
        &self,
        account_id: &AccountId,
    ) -> Result<String, String> {
        let fetched_account = self.get_account_details(account_id).await?;

        // Extract the account commitment from the fetched account
        let commitment = match fetched_account {
            FetchedAccount::Public(_account, summary) => summary.commitment,
            FetchedAccount::Private(_account_id, summary) => summary.commitment,
        };

        Ok(commitment.to_string())
    }

    /// Get the configured RPC client
    pub fn rpc_client(&self) -> &Arc<dyn NodeRpcClient> {
        &self.rpc_client
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

    #[test]
    fn test_client_from_network_type() {
        let network = NetworkType::MidenTestnet;
        let result = MidenNetworkClient::from_network(network);
        assert!(result.is_ok());
    }

}
