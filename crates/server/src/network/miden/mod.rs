pub mod account_inspector;

use crate::metadata::auth::{Auth, Credentials};
use crate::network::miden::account_inspector::MidenAccountInspector;
use crate::network::{NetworkClient, NetworkType};
use async_trait::async_trait;
use miden_objects::Word;
use miden_objects::account::{Account, AccountId};
use miden_objects::crypto::dsa::rpo_falcon512::PublicKey;
use miden_objects::transaction::TransactionSummary;
use miden_objects::transaction::{InputNote, InputNotes, OutputNote, OutputNotes};
use miden_objects::utils::{Deserializable, Serializable};
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
            tracing::error!(
                expected = %account_id.to_hex(),
                actual = %account.id().to_hex(),
                "Account ID mismatch in state JSON"
            );
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
        let account_id = AccountId::from_hex(account_id).map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Invalid Miden account ID format in get_state_commitment"
            );
            format!("Invalid Miden account ID format: {e}")
        })?;

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
        let account_id = AccountId::from_hex(account_id).map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Invalid Miden account ID format in verify_state"
            );
            format!("Invalid Miden account ID format: {e}")
        })?;

        let account = Self::construct_account_from_json(&account_id, state_json)?;
        let local_commitment = account.commitment();
        let local_commitment_hex = format!("0x{}", hex::encode(local_commitment.as_bytes()));

        let on_chain_commitment = self
            .client
            .get_account_commitment(&account_id)
            .await
            .map_err(|e| {
                tracing::error!(
                    account_id = %account_id.to_hex(),
                    error = %e,
                    "Failed to fetch account commitment from Miden network"
                );
                format!("Failed to verify account '{account_id}' on Miden network: {e}")
            })?;

        if local_commitment_hex != on_chain_commitment {
            tracing::error!(
                account_id = %account_id.to_hex(),
                local = %local_commitment_hex,
                on_chain = %on_chain_commitment,
                "Commitment mismatch during state verification"
            );
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
        TransactionSummary::from_json(delta_payload)?;
        let account = Account::from_json(prev_state_json)?;

        let current_commitment = account.commitment();
        let current_commitment_hex = format!("0x{}", hex::encode(current_commitment.as_bytes()));

        if current_commitment_hex != prev_commitment {
            tracing::error!(
                delta_prev_commitment = %prev_commitment,
                state_commitment = %current_commitment_hex,
                "Previous commitment mismatch in verify_delta"
            );
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
        let tx_summary = TransactionSummary::from_json(delta_payload)?;
        let mut account = Account::from_json(prev_state_json)?;
        let inspector = MidenAccountInspector::new(&account);

        let has_psm_auth = inspector.has_psm_auth();

        account
            .apply_delta(tx_summary.account_delta())
            .map_err(|e| {
                tracing::error!(
                    account_id = %account.id().to_hex(),
                    error = %e,
                    "Failed to apply delta to account"
                );
                format!("Failed to apply delta to account: {e}")
            })?;

        if has_psm_auth {
            // Miden multisigs include a map of executed transactions to prevent replay attacks.
            // This affects determinism on simulations as the simulation won't pass the authentication,
            // therefore, the transaction won't be added to the mapping.
            //
            // We need to artificially add the transaction to the mapping
            // to ensure the commitment generated by the new state matches with the commitment
            // generated on-chain when the transaction is executed.
            const EXECUTED_TXS_SLOT: u8 = 2;
            const IS_EXECUTED_FLAG: [u32; 4] = [1, 0, 0, 0];

            let tx_commitment = tx_summary.to_commitment();
            let flag_word = Word::from(IS_EXECUTED_FLAG);

            account
                .storage_mut()
                .set_map_item(EXECUTED_TXS_SLOT, tx_commitment, flag_word)
                .map_err(|e| {
                    tracing::error!(
                        account_id = %account.id().to_hex(),
                        error = %e,
                        "Failed to apply replay protection storage update"
                    );
                    format!("Failed to apply replay protection storage update: {e}")
                })?;

            tracing::debug!(
                account_id = %account.id().to_hex(),
                tx_commitment = %format!("0x{}", hex::encode(tx_commitment.as_bytes())),
                "Applied replay protection adjustment for multisig account"
            );
        }

        let new_commitment = format!("0x{}", hex::encode(account.commitment().as_bytes()));
        let new_state_json = account.to_json();

        Ok((new_state_json, new_commitment))
    }

    fn merge_deltas(
        &self,
        delta_payloads: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        if delta_payloads.is_empty() {
            tracing::error!("Attempted to merge empty delta list");
            return Err("Cannot merge empty delta list".to_string());
        }

        let tx_summaries: Vec<TransactionSummary> = delta_payloads
            .iter()
            .map(TransactionSummary::from_json)
            .collect::<Result<Vec<_>, _>>()?;

        if tx_summaries.is_empty() {
            tracing::error!("No valid deltas to merge after parsing");
            return Err("No valid deltas to merge".to_string());
        }

        // Start with the first TransactionSummary and extract its components
        let first = &tx_summaries[0];
        let mut merged_account_delta = first.account_delta().clone();
        let mut all_input_notes: Vec<InputNote> = first.input_notes().iter().cloned().collect();
        let mut all_output_notes: Vec<OutputNote> = first.output_notes().iter().cloned().collect();

        for tx_summary in tx_summaries.iter().skip(1) {
            all_input_notes.extend(tx_summary.input_notes().iter().cloned());
            all_output_notes.extend(tx_summary.output_notes().iter().cloned());
            merged_account_delta
                .merge(tx_summary.account_delta().clone())
                .map_err(|e| {
                    tracing::error!(
                        error = %e,
                        "Failed to merge account deltas"
                    );
                    format!("Failed to merge account deltas: {e}")
                })?;
        }

        // Create aggregated InputNotes and OutputNotes
        let aggregated_input_notes = InputNotes::new(all_input_notes).map_err(|e| {
            tracing::error!(
                error = %e,
                "Failed to create aggregated input notes"
            );
            format!("Failed to create aggregated input notes: {e}")
        })?;
        let aggregated_output_notes = OutputNotes::new(all_output_notes).map_err(|e| {
            tracing::error!(
                error = %e,
                "Failed to create aggregated output notes"
            );
            format!("Failed to create aggregated output notes: {e}")
        })?;

        // Use the salt from the last TransactionSummary
        // TODO: Maybe we should use a 0 salt to prevent confusions.
        let salt = tx_summaries.last().unwrap().salt();

        // Create the merged TransactionSummary
        let merged_tx_summary = TransactionSummary::new(
            merged_account_delta,
            aggregated_input_notes,
            aggregated_output_notes,
            salt,
        );

        Ok(merged_tx_summary.to_json())
    }

    fn delta_proposal_id(
        &self,
        _account_id: &str,
        _nonce: u64,
        delta_payload: &serde_json::Value,
    ) -> Result<String, String> {
        let tx_summary = TransactionSummary::from_json(delta_payload)?;
        let commitment = tx_summary.to_commitment();

        let proposal_id = format!("0x{}", hex::encode(commitment.as_bytes()));
        Ok(proposal_id)
    }

    fn validate_account_id(&self, account_id: &str) -> Result<(), String> {
        AccountId::from_hex(account_id).map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Invalid Miden account ID format in validate_account_id"
            );
            format!("Invalid Miden account ID format: {e}")
        })?;
        Ok(())
    }

    fn validate_credential(
        &self,
        state_json: &serde_json::Value,
        credential: &Credentials,
    ) -> Result<(), String> {
        let account = Account::from_json(state_json)?;
        let inspector = MidenAccountInspector::new(&account);

        let (credential_pubkey_hex, _signature) = credential.as_signature().ok_or_else(|| {
            tracing::error!("Invalid credential type - expected signature");
            "Invalid credential type".to_string()
        })?;

        let pubkey_bytes = hex::decode(&credential_pubkey_hex[2..]).map_err(|e| {
            tracing::error!(
                pubkey = %credential_pubkey_hex,
                error = %e,
                "Failed to decode credential pubkey"
            );
            format!("Failed to decode credential pubkey: {e}")
        })?;
        let pubkey = PublicKey::read_from_bytes(&pubkey_bytes).map_err(|e| {
            tracing::error!(
                error = %e,
                "Failed to deserialize credential pubkey"
            );
            format!("Failed to deserialize credential pubkey: {e}")
        })?;

        // Compute the commitment to match against storage
        let commitment = pubkey.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));

        if inspector.pubkey_exists(&commitment_hex) {
            Ok(())
        } else {
            tracing::error!(
                commitment = %commitment_hex,
                "Credential public key commitment not found in account storage"
            );
            Err(format!(
                "Credential public key commitment '{}...' not found in account storage",
                &commitment_hex[..18]
            ))
        }
    }

    async fn should_update_auth(
        &mut self,
        state_json: &serde_json::Value,
    ) -> Result<Option<Auth>, String> {
        let account = Account::from_json(state_json)?;
        let inspector = MidenAccountInspector::new(&account);

        let commitments = inspector.extract_slot_1_pubkeys();

        if commitments.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Auth::MidenFalconRpo {
                cosigner_commitments: commitments,
            }))
        }
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
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
