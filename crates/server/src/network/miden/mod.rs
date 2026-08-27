pub mod account_inspector;

use crate::metadata::auth::{Auth, Credentials};
use crate::network::miden::account_inspector::{
    MidenAccountInspector, guardian_public_key_slot_name,
};
use crate::network::{
    MidenRpcSettings, NetworkClient, NetworkType, RpcReadMode, StateVerification,
};
use async_trait::async_trait;
use guardian_shared::{FromJson, ToJson};
use miden_protocol::Word;
use miden_protocol::account::{
    Account, AccountId, AccountStoragePatch, StorageMapKey, StorageMapPatch,
    StorageMapPatchEntries, StorageSlotPatch,
};
use miden_protocol::transaction::{
    InputNote, InputNotes, RawOutputNote, RawOutputNotes, TransactionSummary,
};
use miden_rpc_client::MidenRpcClient;
use miden_standards::account::auth::AuthGuardedMultisig;

/// Miden network client for fetching on-chain account data
pub struct MidenNetworkClient {
    client: MidenRpcClient,
}

impl MidenNetworkClient {
    /// Create a new Miden network client from a NetworkType
    pub async fn from_network(network: NetworkType) -> Result<Self, String> {
        Self::from_settings(&MidenRpcSettings::from_env(network)?).await
    }

    /// Create a new Miden network client from resolved RPC settings.
    pub(crate) async fn from_settings(settings: &MidenRpcSettings) -> Result<Self, String> {
        let mut client = MidenRpcClient::connect_with_settings(
            settings.endpoint().expose_secret(),
            settings.client_settings(),
        )
        .await
        .map_err(|e| e.to_string())?;
        client.set_retry_observer(std::sync::Arc::new(|operation| {
            metrics::counter!(
                crate::metrics::names::MIDEN_RPC_RETRIES_TOTAL,
                crate::metrics::names::LABEL_OPERATION => operation
            )
            .increment(1);
        }));
        Ok(Self { client })
    }

    /// Builds a client without contacting the network or loading TLS roots, for
    /// unit tests that exercise the pure serialization/delta paths
    /// (`get_state_commitment`, `validate_guardian_commitment`, `apply_delta`)
    /// which never issue an RPC.
    #[cfg(all(test, any(feature = "e2e", not(feature = "integration"))))]
    pub(crate) fn lazy_for_test(network: NetworkType) -> Self {
        let client = MidenRpcClient::lazy_unconnected(network.rpc_endpoint())
            .expect("lazy client construction is infallible for a valid endpoint");
        Self { client }
    }

    /// True when an on-chain commitment is the empty-word digest the Miden
    /// node reports for an account it has never seen — i.e. the account's
    /// first transaction has not landed yet. This sentinel is a Miden
    /// protocol detail; it is translated to [`StateVerification::Absent`]
    /// here so shared layers never interpret raw digests.
    fn is_empty_word_digest(on_chain: &str) -> bool {
        let digest = on_chain.strip_prefix("0x").unwrap_or(on_chain);
        !digest.is_empty() && digest.bytes().all(|b| b == b'0')
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
        let local_commitment = account.to_commitment();
        let local_commitment_hex = format!("0x{}", hex::encode(local_commitment.as_bytes()));

        Ok(local_commitment_hex)
    }

    async fn verify_commitment(
        &self,
        account_id: &str,
        expected_commitment: &str,
        read_mode: RpcReadMode,
    ) -> Result<StateVerification, String> {
        let account_id = AccountId::from_hex(account_id).map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Invalid Miden account ID format in verify_commitment"
            );
            format!("Invalid Miden account ID format: {e}")
        })?;

        // Outbound chain-node RPC — the upstream dependency this
        // server's availability hangs on, so it gets its own metric
        // (`operation` is the static RPC method name, a closed set).
        let rpc_started = std::time::Instant::now();
        let rpc_result = self
            .client
            .get_account_commitment(&account_id, read_mode)
            .await;
        metrics::counter!(
            crate::metrics::names::MIDEN_RPC_REQUESTS_TOTAL,
            crate::metrics::names::LABEL_OPERATION => "get_account_commitment",
            crate::metrics::names::LABEL_OUTCOME =>
                crate::metrics::labels::Outcome::from_ok(rpc_result.is_ok()).as_str()
        )
        .increment(1);
        metrics::histogram!(
            crate::metrics::names::MIDEN_RPC_DURATION_SECONDS,
            crate::metrics::names::LABEL_OPERATION => "get_account_commitment"
        )
        .record(rpc_started.elapsed().as_secs_f64());

        let on_chain_commitment = rpc_result.map_err(|e| {
            tracing::error!(
                account_id = %account_id.to_hex(),
                error = %e,
                "Failed to fetch account commitment from Miden network"
            );
            format!("Failed to verify account '{account_id}' on Miden network: {e}")
        })?;

        if Self::is_empty_word_digest(&on_chain_commitment) {
            return Ok(StateVerification::Absent);
        }

        if expected_commitment != on_chain_commitment {
            tracing::debug!(
                account_id = %account_id.to_hex(),
                expected = %expected_commitment,
                on_chain = %on_chain_commitment,
                "Commitment mismatch during state verification"
            );
            return Ok(StateVerification::Mismatch {
                on_chain: on_chain_commitment,
            });
        }

        Ok(StateVerification::Match)
    }

    fn verify_delta(
        &self,
        prev_commitment: &str,
        prev_state_json: &serde_json::Value,
        delta_payload: &serde_json::Value,
    ) -> Result<(), String> {
        TransactionSummary::from_json(delta_payload)?;
        let account = Account::from_json(prev_state_json)?;

        let current_commitment = account.to_commitment();
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
        let account_delta = tx_summary.account_delta();

        let is_full_state = account_delta.is_full_state();
        let base_account = if is_full_state {
            tracing::debug!(
                account_id = %account_delta.id().to_hex(),
                "Processing full state delta for new account deployment"
            );
            guardian_shared::account_delta::account_from_full_delta_with_storage_patch(
                account_delta,
                AccountStoragePatch::new(),
            )?
        } else {
            Account::from_json(prev_state_json)?
        };

        // Authentication records replay protection from the pre-delta account shape.
        // A delta may add or remove the multisig component itself.
        let is_multisig = MidenAccountInspector::new(&base_account).has_multisig_auth();
        let mut storage_entries = Vec::new();

        if is_multisig {
            // Miden multisigs include a map of executed transactions to prevent replay attacks.
            // This affects determinism on simulations as the simulation won't pass the authentication,
            // therefore, the transaction won't be added to the mapping.
            //
            // We need to artificially add the transaction to the mapping
            // to ensure the commitment generated by the new state matches with the commitment
            // generated on-chain when the transaction is executed.
            const IS_EXECUTED_FLAG: [u32; 4] = [1, 0, 0, 0];

            let tx_commitment = tx_summary.to_commitment();
            let flag_word = Word::from(IS_EXECUTED_FLAG);

            let slot_name = AuthGuardedMultisig::executed_transactions_slot().clone();

            let entries =
                StorageMapPatchEntries::from_iter([(StorageMapKey::new(tx_commitment), flag_word)]);
            storage_entries.push((
                slot_name,
                StorageSlotPatch::Map(StorageMapPatch::Update { entries }),
            ));

            tracing::debug!(
                account_id = %base_account.id().to_hex(),
                tx_commitment = %format!("0x{}", hex::encode(tx_commitment.as_bytes())),
                "Applied replay protection adjustment for multisig account"
            );
        }

        let additional_storage = AccountStoragePatch::from_entries(storage_entries)
            .map_err(|error| format!("Failed to build storage adjustment patch: {error}"))?;

        let account = if is_full_state {
            guardian_shared::account_delta::account_from_full_delta_with_storage_patch(
                account_delta,
                additional_storage,
            )?
        } else {
            let mut account = base_account;
            guardian_shared::account_delta::apply_account_delta_with_storage_patch(
                &mut account,
                account_delta,
                additional_storage,
            )?;
            account
        };

        let new_commitment = format!("0x{}", hex::encode(account.to_commitment().as_bytes()));
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
        let mut all_output_notes: Vec<RawOutputNote> =
            first.output_notes().iter().cloned().collect();

        for tx_summary in tx_summaries.iter().skip(1) {
            all_input_notes.extend(tx_summary.input_notes().iter().cloned());
            all_output_notes.extend(tx_summary.output_notes().iter().cloned());
            merged_account_delta =
                merge_account_deltas(merged_account_delta, tx_summary.account_delta().clone())
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
        let aggregated_output_notes = RawOutputNotes::new(all_output_notes).map_err(|e| {
            tracing::error!(
                error = %e,
                "Failed to create aggregated output notes"
            );
            format!("Failed to create aggregated output notes: {e}")
        })?;

        // Carry the reference block, expiration delta and user params (which hold the
        // auth-arg salt) from the last TransactionSummary, matching the pre-existing
        // salt convention.
        let last = tx_summaries.last().unwrap();
        let block_commitment = last.block_commitment();
        let expiration_delta = last.expiration_delta();
        let user_params = last.user_params();

        // Create the merged TransactionSummary
        let merged_tx_summary = TransactionSummary::new(
            merged_account_delta,
            aggregated_input_notes,
            aggregated_output_notes,
            block_commitment,
            expiration_delta,
            user_params,
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
        auth: &Auth,
    ) -> Result<(), String> {
        let account = Account::from_json(state_json)?;
        let inspector = MidenAccountInspector::new(&account);

        let (credential_pubkey_hex, _signature, _timestamp) =
            credential.as_signature().ok_or_else(|| {
                tracing::error!("Invalid credential type - expected signature");
                "Invalid credential type".to_string()
            })?;

        let commitment_hex = auth.compute_signer_commitment(credential_pubkey_hex)?;

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

    fn validate_guardian_commitment(
        &self,
        state_json: &serde_json::Value,
        expected_guardian_commitment: &str,
    ) -> Result<(), String> {
        let account = Account::from_json(state_json)?;
        let inspector = MidenAccountInspector::new(&account);

        let guardian_slot = guardian_public_key_slot_name();
        let actual_guardian_commitment = inspector
            .extract_guardian_public_key()
            .ok_or_else(|| format!("Missing required slot '{guardian_slot}'"))?;

        if actual_guardian_commitment == expected_guardian_commitment {
            Ok(())
        } else {
            Err(format!(
                "Slot '{guardian_slot}' mismatch: expected {expected_guardian_commitment}, got {actual_guardian_commitment}"
            ))
        }
    }

    fn extract_guardian_commitment(
        &self,
        state_json: &serde_json::Value,
    ) -> Result<Option<String>, String> {
        let account = Account::from_json(state_json)?;
        let inspector = MidenAccountInspector::new(&account);
        Ok(inspector.extract_guardian_public_key())
    }

    async fn should_update_auth(
        &self,
        state_json: &serde_json::Value,
        current_auth: &Auth,
    ) -> Result<Option<Auth>, String> {
        let account = Account::from_json(state_json)?;
        let inspector = MidenAccountInspector::new(&account);

        let commitments = inspector.extract_slot_1_pubkeys();

        if commitments.is_empty() {
            Ok(None)
        } else {
            Ok(Some(current_auth.with_updated_commitments(commitments)))
        }
    }
}

/// Merges two relative account deltas into one, replacing the upstream
/// `AccountDelta::merge` removed in Miden 0.16: storage patches merge
/// natively, vault deltas accumulate asset-by-asset, and nonce deltas add.
/// Only the first delta in a merge sequence may carry account code.
fn merge_account_deltas(
    base: miden_protocol::account::AccountDelta,
    next: miden_protocol::account::AccountDelta,
) -> Result<miden_protocol::account::AccountDelta, String> {
    let account_id = base.id();
    let (mut storage, mut vault, code, nonce_delta) = base.into_parts();
    let (next_storage, next_vault, next_code, next_nonce_delta) = next.into_parts();

    if next_code.is_some() {
        return Err("unexpected full-state delta after the first delta in a merge".to_string());
    }

    storage
        .merge(next_storage)
        .map_err(|e| format!("failed to merge storage patches: {e}"))?;
    for asset in next_vault.added_assets() {
        vault
            .add_asset(asset)
            .map_err(|e| format!("failed to merge added asset: {e}"))?;
    }
    for asset in next_vault.removed_assets() {
        vault
            .remove_asset(asset)
            .map_err(|e| format!("failed to merge removed asset: {e}"))?;
    }

    miden_protocol::account::AccountDelta::new(
        account_id,
        storage,
        vault,
        code,
        nonce_delta + next_nonce_delta,
    )
    .map_err(|e| format!("failed to build merged delta: {e}"))
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use miden_protocol::transaction::TransactionSummaryUserParams;

    use super::*;

    #[test]
    fn test_network_type_rpc_endpoint() {
        let network = NetworkType::MidenTestnet;
        assert_eq!(network.rpc_endpoint(), "https://rpc.testnet.miden.io");
    }

    #[test]
    fn test_empty_word_digest_detection() {
        assert!(MidenNetworkClient::is_empty_word_digest(
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        ));
        assert!(MidenNetworkClient::is_empty_word_digest(
            "0000000000000000000000000000000000000000000000000000000000000000"
        ));
        assert!(!MidenNetworkClient::is_empty_word_digest(
            "0x0000000000000000000000000000000000000000000000000000000000000001"
        ));
        assert!(!MidenNetworkClient::is_empty_word_digest(""));
        assert!(!MidenNetworkClient::is_empty_word_digest("0x"));
    }

    #[tokio::test]
    #[ignore = "requires live network access and system TLS roots; covered by integration suites"]
    async fn test_client_from_network_type() {
        let network = NetworkType::MidenTestnet;
        let result = MidenNetworkClient::from_network(network).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_state_commitment_invalid_state_json() {
        let network = NetworkType::MidenTestnet;
        let client = MidenNetworkClient::lazy_for_test(network);

        let account_id_hex = "0x8a8a8a8a8a8a8a010a8a8a8a8a8a8a";
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
        let client = MidenNetworkClient::lazy_for_test(network);

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

    #[tokio::test]
    async fn test_verify_commitment_rejects_invalid_account_id_before_rpc() {
        let client = MidenNetworkClient::lazy_for_test(NetworkType::MidenTestnet);

        let result = client
            .verify_commitment("not_a_valid_hex", "0xexpected", RpcReadMode::SingleAttempt)
            .await;

        assert!(result.is_err(), "Should fail with invalid account ID");
        assert!(
            result
                .unwrap_err()
                .contains("Invalid Miden account ID format")
        );
    }

    #[tokio::test]
    async fn test_validate_guardian_commitment_success() {
        let network = NetworkType::MidenTestnet;
        let client = MidenNetworkClient::lazy_for_test(network);

        let account_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse account fixture");

        let account =
            Account::from_json(&account_json).expect("Failed to deserialize fixture account");
        let inspector = MidenAccountInspector::new(&account);
        let expected_guardian_commitment = inspector
            .extract_guardian_public_key()
            .expect("Fixture must contain OpenZeppelin GUARDIAN public key slot");

        let result =
            client.validate_guardian_commitment(&account_json, &expected_guardian_commitment);
        assert!(
            result.is_ok(),
            "Expected matching GUARDIAN commitment to pass"
        );
    }

    #[tokio::test]
    async fn test_validate_guardian_commitment_mismatch() {
        let network = NetworkType::MidenTestnet;
        let client = MidenNetworkClient::lazy_for_test(network);

        let account_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse account fixture");

        let result = client.validate_guardian_commitment(
            &account_json,
            "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        );
        assert!(
            result.is_err(),
            "Expected mismatched GUARDIAN commitment to fail"
        );
        assert!(
            result
                .unwrap_err()
                .contains(guardian_public_key_slot_name()),
            "Error should mention the required guardian pub_key slot name"
        );
    }

    #[tokio::test]
    async fn test_apply_delta() {
        let network = NetworkType::MidenTestnet;
        let client = MidenNetworkClient::lazy_for_test(network);

        let account_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse account fixture");

        let delta_fixture: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::DELTA_1_JSON)
                .expect("Failed to parse delta fixture");

        let delta_payload = delta_fixture
            .get("delta_payload")
            .expect("delta_payload field missing");

        let expected_commitment = delta_fixture
            .get("new_commitment")
            .and_then(serde_json::Value::as_str)
            .expect("new_commitment field missing");

        let (new_state_json, new_commitment) = client
            .apply_delta(&account_json, delta_payload)
            .expect("apply_delta should succeed");

        assert_eq!(
            new_commitment, expected_commitment,
            "Commitment after apply_delta should match expected"
        );

        assert!(
            new_state_json.get("data").is_some(),
            "New state should have data field"
        );
    }

    #[tokio::test]
    async fn test_apply_delta_full_state() {
        use miden_protocol::Felt;
        use miden_protocol::account::AccountDelta;
        use miden_protocol::account::delta::AccountVaultDelta;
        use miden_protocol::account::{AccountBuilder, AccountType};
        use miden_standards::account::auth::NoAuth;
        use miden_standards::account::wallets::BasicWallet;

        let network = NetworkType::MidenTestnet;
        let client = MidenNetworkClient::lazy_for_test(network);

        // Create a simple account without GUARDIAN auth to test the full state delta path
        // This avoids the replay protection logic which requires proper storage maps
        let account = AccountBuilder::new([0xAB; 32])
            .account_type(AccountType::Public)
            .with_component(BasicWallet)
            .with_component(NoAuth)
            .build()
            .expect("Failed to build account");

        // Create a full state delta by using with_code() to add code to the delta
        // This simulates a new account deployment where the full account state is included
        // A full state delta has code attached, which distinguishes it from a partial update
        let full_state_delta = AccountDelta::new(
            account.id(),
            miden_protocol::account::AccountStoragePatch::default(),
            AccountVaultDelta::default(),
            Some(account.code().clone()),
            Felt::new_unchecked(1),
        )
        .expect("Failed to create delta");

        // Verify this is indeed a full state delta
        assert!(
            full_state_delta.is_full_state(),
            "Delta should be a full state delta"
        );

        // Create a TransactionSummary with the full state delta
        let tx_summary = TransactionSummary::new(
            full_state_delta,
            InputNotes::new(Vec::new()).expect("empty input notes"),
            RawOutputNotes::new(Vec::new()).expect("empty output notes"),
            Word::default(),
            0,
            TransactionSummaryUserParams::new([Felt::ZERO; 7]),
        );

        let delta_payload = tx_summary.to_json();

        // Full-state deltas read prev_state_json only for the pre-tx guardian
        // selector; an unparseable prev state falls back to guardian-disabled
        // (with a warn), which is fine here — this account has no guardian
        // component, so the empty prev state exercises exactly that fallback.
        let empty_prev_state = serde_json::json!({});

        let (new_state_json, new_commitment) = client
            .apply_delta(&empty_prev_state, &delta_payload)
            .expect("apply_delta with full state should succeed");

        // The new state should have a data field
        assert!(
            new_state_json.get("data").is_some(),
            "New state from full delta should have data field"
        );

        // Commitment should be a valid hex string
        assert!(
            new_commitment.starts_with("0x"),
            "Commitment should be hex format"
        );
        assert_eq!(
            new_commitment.len(),
            66,
            "Commitment should be 32 bytes (64 hex chars + 0x prefix)"
        );
    }
}
