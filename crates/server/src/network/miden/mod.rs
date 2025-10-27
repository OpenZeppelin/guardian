pub mod account_inspector;

use crate::metadata::auth::{Auth, Credentials};
use crate::network::miden::account_inspector::MidenAccountInspector;
use crate::network::{NetworkClient, NetworkType};
use async_trait::async_trait;
use miden_objects::account::{Account, AccountDelta, AccountId};
use miden_rpc_client::MidenRpcClient;
use private_state_manager_shared::{FromJson, ToJson};

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

    /// Construct an Account object from JSON state representation
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

#[async_trait]
impl NetworkClient for MidenNetworkClient {
    fn get_state_commitment(
        &self,
        account_id: &str,
        state_json: &serde_json::Value,
    ) -> Result<String, String> {
        let account_id = AccountId::from_hex(account_id)
            .map_err(|e| format!("Invalid Miden account ID format: {e}"))?;

        let account = Self::construct_account_from_json(&account_id, state_json)?;
        let local_commitment = account.commitment();
        let local_commitment_hex = format!("0x{}", hex::encode(local_commitment.as_bytes()));

        Ok(local_commitment_hex)
    }

    async fn verify_state(
        &mut self,
        account_id: &str,
        state_json: &serde_json::Value,
    ) -> Result<(), String> {
        let account_id = AccountId::from_hex(account_id)
            .map_err(|e| format!("Invalid Miden account ID format: {e}"))?;

        let account = Self::construct_account_from_json(&account_id, state_json)?;
        let local_commitment = account.commitment();
        let local_commitment_hex = format!("0x{}", hex::encode(local_commitment.as_bytes()));

        let on_chain_commitment = self
            .client
            .get_account_commitment(&account_id)
            .await
            .map_err(|e| {
                format!("Failed to verify account '{account_id}' on Miden network: {e}")
            })?;

        if local_commitment_hex != on_chain_commitment {
            return Err(format!(
                "Commitment mismatch for account '{account_id}': local={local_commitment_hex}, on-chain={on_chain_commitment}"
            ));
        }

        Ok(())
    }

    fn verify_delta(
        &self,
        prev_commitment: &str,
        prev_state_json: &serde_json::Value,
        delta_payload: &serde_json::Value,
    ) -> Result<(), String> {
        AccountDelta::from_json(delta_payload)?;
        let account = Account::from_json(prev_state_json)?;

        let current_commitment = account.commitment();
        let current_commitment_hex = format!("0x{}", hex::encode(current_commitment.as_bytes()));

        if current_commitment_hex != prev_commitment {
            return Err(format!(
                "Previous commitment mismatch: delta specifies {prev_commitment}, but current state has {current_commitment_hex}"
            ));
        }

        Ok(())
    }

    fn apply_delta(
        &self,
        prev_state_json: &serde_json::Value,
        delta_payload: &serde_json::Value,
    ) -> Result<(serde_json::Value, String), String> {
        let delta = AccountDelta::from_json(delta_payload)?;
        let mut account = Account::from_json(prev_state_json)?;

        account
            .apply_delta(&delta)
            .map_err(|e| format!("Failed to apply delta to account: {e}"))?;

        let new_commitment = format!("0x{}", hex::encode(account.commitment().as_bytes()));
        let new_state_json = account.to_json();

        Ok((new_state_json, new_commitment))
    }

    fn merge_deltas(
        &self,
        delta_payloads: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        if delta_payloads.is_empty() {
            return Err("Cannot merge empty delta list".to_string());
        }

        let mut deltas: Vec<AccountDelta> = delta_payloads
            .iter()
            .map(AccountDelta::from_json)
            .collect::<Result<Vec<_>, _>>()?;

        if deltas.is_empty() {
            return Err("No valid deltas to merge".to_string());
        }

        let mut merged = deltas.remove(0);

        for delta in deltas {
            merged
                .merge(delta)
                .map_err(|e| format!("Failed to merge deltas: {e}"))?;
        }

        Ok(merged.to_json())
    }

    fn validate_account_id(&self, account_id: &str) -> Result<(), String> {
        AccountId::from_hex(account_id)
            .map_err(|e| format!("Invalid Miden account ID format: {e}"))?;
        Ok(())
    }

    fn validate_credential(
        &self,
        state_json: &serde_json::Value,
        credential: &Credentials,
    ) -> Result<(), String> {
        let account = Account::from_json(state_json)?;
        let inspector = MidenAccountInspector::new(&account);

        let (credential_pubkey, _signature) = credential
            .as_signature()
            .ok_or_else(|| "Invalid credential type".to_string())?;

        if inspector.pubkey_exists(credential_pubkey) {
            Ok(())
        } else {
            Err(format!(
                "Credential public key '{credential_pubkey}' not found in account storage"
            ))
        }
    }

    async fn should_update_auth(
        &mut self,
        state_json: &serde_json::Value,
    ) -> Result<Option<Auth>, String> {
        let account = Account::from_json(state_json)?;
        let inspector = MidenAccountInspector::new(&account);

        let pubkeys = inspector.extract_slot_1_pubkeys();

        if pubkeys.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Auth::MidenFalconRpo {
                cosigner_pubkeys: pubkeys,
            }))
        }
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
    async fn test_get_state_commitment_invalid_state_json() {
        let network = NetworkType::MidenTestnet;
        let client = MidenNetworkClient::from_network(network)
            .await
            .expect("Failed to create client");

        let account_id_hex = "0x8a65fc5a39e4cd106d648e3eb4ab5f";
        let state_json = serde_json::json!({"balance": 0});

        let result = client.get_state_commitment(account_id_hex, &state_json);
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
    async fn test_get_state_commitment_invalid_format() {
        let network = NetworkType::MidenTestnet;
        let client = MidenNetworkClient::from_network(network)
            .await
            .expect("Failed to create client");

        let invalid_account_id = "not_a_valid_hex";
        let state_json = serde_json::json!({"balance": 0});

        let result = client.get_state_commitment(invalid_account_id, &state_json);
        assert!(result.is_err(), "Should fail with invalid account ID");
        assert!(
            result
                .unwrap_err()
                .contains("Invalid Miden account ID format")
        );
    }
}
