pub mod account_inspector;

use crate::metadata::auth::{Auth, Credentials};
use crate::network::miden::account_inspector::{
    MidenAccountInspector, guardian_public_key_slot_name,
};
use crate::network::{
    AppliedState, MidenRpcSettings, NetworkClient, NetworkType, OnChainGuardianBinding,
    RpcReadMode, StateVerification, TransactionSearch,
};
use crate::state_object::StateHead;
use async_trait::async_trait;
use guardian_shared::{FromJson, ToJson};
use std::collections::BTreeMap;

use miden_protocol::Word;
use miden_protocol::account::delta::{AssetDelta, AssetDeltaOperation};
use miden_protocol::account::{
    Account, AccountDelta, AccountId, AccountStoragePatch, AccountVaultDelta, StorageMapKey,
    StorageMapPatch, StorageMapPatchEntries, StorageSlotPatch,
};
use miden_protocol::asset::{Asset, AssetId, FungibleAsset};
use miden_protocol::transaction::{
    InputNote, InputNotes, RawOutputNote, RawOutputNotes, TransactionSummary,
};
use miden_rpc_client::{MidenRpcClient, primitives, rpc};
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
    /// tests that exercise the pure serialization/delta paths
    /// (`get_state_head`, `validate_guardian_commitment`, `apply_delta`,
    /// `account_nonce`) which never issue an RPC: unit tests directly, and the
    /// integration and e2e suites through `IntegrationMockNetworkClient`.
    #[cfg(test)]
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

    /// Hex encoding of a node-reported word — the one encoding the RPC
    /// client uses for commitments, so storage words and commitments
    /// compare byte-for-byte with `Word::to_bytes()` / `Word::as_bytes()`.
    /// A payload other than 32 bytes is a malformed answer; in particular
    /// proto3's default (empty) word is never the zero word.
    fn word_hex(word: &primitives::Word, what: &str) -> Result<String, String> {
        miden_rpc_client::word_to_hex(word).ok_or_else(|| {
            format!(
                "malformed {what}: {} bytes, expected the 32 bytes of a word",
                word.encoded.len()
            )
        })
    }

    /// True for the zero word as the node encodes it: 32 zero bytes.
    fn is_zero_word(word: &primitives::Word) -> bool {
        word.encoded.len() == 32 && word.encoded.iter().all(|byte| *byte == 0)
    }

    /// The storage-detail request that asks the node for the guardian
    /// public key map only: one small map, all entries, no code, no vault.
    fn guardian_map_detail_request() -> rpc::get_account_request::AccountDetailRequest {
        use rpc::get_account_request::account_detail_request::storage_map_detail_request::SlotData;
        use rpc::get_account_request::account_detail_request::{
            StorageMapDetailRequest, StorageMapDetailRequests, StorageRequest,
        };
        rpc::get_account_request::AccountDetailRequest {
            code_commitment: None,
            asset_vault_commitment: None,
            storage_request: Some(StorageRequest::StorageMaps(StorageMapDetailRequests {
                storage_maps: vec![StorageMapDetailRequest {
                    slot_name: guardian_public_key_slot_name().to_string(),
                    slot_data: Some(SlotData::AllEntries(true)),
                }],
            })),
        }
    }

    /// Interpret a `GetAccount` response that requested the guardian map
    /// (see [`Self::guardian_map_detail_request`]). Pure so the response
    /// shapes can be unit-tested without a node.
    ///
    /// The guardian key lives at map key `ZERO` of the guardian pub_key
    /// slot, exactly where [`MidenAccountInspector::extract_guardian_public_key`]
    /// reads it in client-supplied state; a zero value means "no binding"
    /// there too. The node answers `AllEntries` with the original
    /// (unhashed) map keys, and encodes `ZERO` as 32 zero bytes. A
    /// response without details is a private account. Details that carry
    /// no storage answer, and malformed words, are errors, not "no
    /// binding": nothing can be concluded from them.
    fn guardian_binding_from_response(
        response: &rpc::GetAccountResponse,
    ) -> Result<OnChainGuardianBinding, String> {
        use rpc::account_storage_details::account_storage_map_details::Result as MapResult;

        let witness_commitment = response
            .witness
            .as_ref()
            .and_then(|witness| witness.commitment.as_ref())
            .ok_or_else(|| "no commitment in account witness".to_string())?;
        let on_chain_commitment = Self::word_hex(witness_commitment, "witness commitment")?;

        let Some(details) = response.details.as_ref() else {
            return Ok(OnChainGuardianBinding::Opaque);
        };
        let block_num = response
            .block_num
            .as_ref()
            .map(|number| number.block_num)
            .ok_or_else(|| "account response carries no block number".to_string())?;
        let nonce = details
            .header
            .as_ref()
            .map(|header| header.nonce)
            .ok_or_else(|| "account details carry no header".to_string())?;
        // Details without a storage answer mean the node did not answer
        // the storage request: nothing can be concluded about the slot,
        // so this is a failed read, not "no binding".
        let guardian_slot = guardian_public_key_slot_name();
        let storage = details
            .storage_details
            .as_ref()
            .ok_or_else(|| "account details carry no storage answer".to_string())?;
        let map = storage
            .map_details
            .iter()
            .find(|map| map.slot_name == guardian_slot);
        let guardian_commitment = match map {
            // No answer for the requested map. If the storage header lists
            // the slot, the node skipped it and nothing can be concluded;
            // otherwise the state carries no guardian slot at all.
            None if storage.header.as_ref().is_some_and(|header| {
                header
                    .slots
                    .iter()
                    .any(|slot| slot.slot_name == guardian_slot)
            }) =>
            {
                return Err(format!(
                    "node did not answer the requested guardian map '{guardian_slot}'"
                ));
            }
            None => None,
            Some(map) => match map.result.as_ref() {
                None => {
                    return Err(format!(
                        "node answered the guardian map '{guardian_slot}' without a result"
                    ));
                }
                Some(MapResult::AllEntries(entries)) => {
                    let mut key_zero = None;
                    for entry in &entries.entries {
                        let key = entry.key.as_ref().ok_or_else(|| {
                            format!("guardian map '{guardian_slot}' has an entry without a key")
                        })?;
                        Self::word_hex(key, "guardian map key")?;
                        if Self::is_zero_word(key) {
                            key_zero = Some(entry);
                        }
                    }
                    match key_zero {
                        // No entry at key zero: no binding.
                        None => None,
                        // An entry without a value is a malformed answer,
                        // not evidence of anything.
                        Some(entry) => {
                            let value = entry.value.as_ref().ok_or_else(|| {
                                format!("guardian map '{guardian_slot}' has no value for key zero")
                            })?;
                            let value_hex = Self::word_hex(value, "guardian key")?;
                            (!Self::is_zero_word(value)).then_some(value_hex)
                        }
                    }
                }
                Some(MapResult::TooManyEntries(_)) => {
                    return Err(format!(
                        "guardian map '{guardian_slot}' exceeds the node's entry limit"
                    ));
                }
                Some(MapResult::PartialMap(_)) => {
                    return Err(format!(
                        "node answered the guardian map '{guardian_slot}' with a partial \
                         map although all entries were requested"
                    ));
                }
            },
        };
        Ok(OnChainGuardianBinding::Visible {
            on_chain_commitment,
            guardian_commitment,
            nonce,
            block_num,
        })
    }

    /// Scan one `SyncTransactions` page for a transaction whose final
    /// state commitment is `target`. Returns the block of the first hit
    /// and the last block the page fully covers (the node paginates by
    /// block). Only the final commitment is compared: a new account's
    /// first transaction records an empty initial commitment. Pure so page
    /// shapes can be unit-tested without a node.
    fn scan_transaction_page(
        page: &rpc::SyncTransactionsResponse,
        target: &str,
    ) -> Result<(Option<u32>, u32), String> {
        let covered_through = page
            .pagination_info
            .as_ref()
            .map(|info| info.block_num)
            .ok_or_else(|| "transaction page carries no pagination info".to_string())?;
        for record in &page.transactions {
            let final_commitment = record
                .header
                .as_ref()
                .and_then(|header| header.final_state_commitment.as_ref())
                .ok_or_else(|| {
                    format!(
                        "transaction in block {} has no final state commitment",
                        record.block_num
                    )
                })?;
            let final_hex = Self::word_hex(final_commitment, "final state commitment")?;
            if final_hex.eq_ignore_ascii_case(target) {
                return Ok((Some(record.block_num), covered_through));
            }
        }
        Ok((None, covered_through))
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

    fn ensure_prev_commitment(account: &Account, prev_commitment: &str) -> Result<(), String> {
        let current_commitment = account.to_commitment();
        let current_commitment_hex = format!("0x{}", hex::encode(current_commitment.as_bytes()));

        if current_commitment_hex != prev_commitment {
            tracing::error!(
                delta_prev_commitment = %prev_commitment,
                state_commitment = %current_commitment_hex,
                "Delta base commitment does not match the stored state"
            );
            return Err(format!(
                "Previous commitment mismatch: delta specifies {prev_commitment}, but current state has {current_commitment_hex}"
            ));
        }

        Ok(())
    }

    fn apply_summary(
        prev_account: Result<Account, String>,
        tx_summary: &TransactionSummary,
    ) -> Result<AppliedState, String> {
        let account_delta = tx_summary.account_delta();

        let is_full_state =
            !account_delta.code().is_empty() && prev_account.as_ref().map_or(true, Account::is_new);
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
            prev_account?
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

        let inspector = MidenAccountInspector::new(&account);
        Ok(AppliedState {
            commitment: format!("0x{}", hex::encode(account.to_commitment().as_bytes())),
            nonce: Some(account.nonce().as_canonical_u64()),
            cosigner_commitments: inspector.extract_slot_1_pubkeys(),
            guardian_commitment: inspector.extract_guardian_public_key(),
            state_json: account.to_json(),
        })
    }
}

#[async_trait]
impl NetworkClient for MidenNetworkClient {
    fn get_state_head(
        &self,
        account_id: &str,
        state_json: &serde_json::Value,
    ) -> Result<StateHead, String> {
        let account_id = AccountId::from_hex(account_id).map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Invalid Miden account ID format in get_state_head"
            );
            format!("Invalid Miden account ID format: {e}")
        })?;

        let account = Self::construct_account_from_json(&account_id, state_json)?;
        let local_commitment = account.to_commitment();
        let local_commitment_hex = format!("0x{}", hex::encode(local_commitment.as_bytes()));

        Ok(StateHead {
            commitment: local_commitment_hex,
            nonce: Some(account.nonce().as_canonical_u64()),
        })
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
        Self::ensure_prev_commitment(&account, prev_commitment)
    }

    fn apply_delta(
        &self,
        prev_state_json: &serde_json::Value,
        delta_payload: &serde_json::Value,
    ) -> Result<AppliedState, String> {
        let tx_summary = TransactionSummary::from_json(delta_payload)?;
        Self::apply_summary(Account::from_json(prev_state_json), &tx_summary)
    }

    fn verify_and_apply_delta(
        &self,
        prev_commitment: &str,
        prev_state_json: &serde_json::Value,
        delta_payload: &serde_json::Value,
    ) -> Result<AppliedState, String> {
        let tx_summary = TransactionSummary::from_json(delta_payload)?;
        let account = Account::from_json(prev_state_json)?;
        Self::ensure_prev_commitment(&account, prev_commitment)?;
        Self::apply_summary(Ok(account), &tx_summary)
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

        let merged_account_delta =
            merge_account_deltas(tx_summaries.iter().map(TransactionSummary::account_delta))
                .map_err(|e| {
                    tracing::error!(
                        error = %e,
                        "Failed to merge account deltas"
                    );
                    format!("Failed to merge account deltas: {e}")
                })?;
        let all_input_notes: Vec<InputNote> = tx_summaries
            .iter()
            .flat_map(|tx_summary| tx_summary.input_notes().iter().cloned())
            .collect();
        let all_output_notes: Vec<RawOutputNote> = tx_summaries
            .iter()
            .flat_map(|tx_summary| tx_summary.output_notes().iter().cloned())
            .collect();

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

        // Carry the bound block, expiration delta and user params from the last
        // TransactionSummary. Since Miden 0.17 the user params hold the approval
        // expiration and the salt the multisig binds; they are carried through
        // opaquely.
        let last = tx_summaries.last().unwrap();
        let block_number = last.block_number();
        let block_commitment = last.block_commitment();
        let expiration_delta = last.expiration_delta();
        let user_params = last.user_params();

        // Create the merged TransactionSummary
        let merged_tx_summary = TransactionSummary::new(
            merged_account_delta,
            aggregated_input_notes,
            aggregated_output_notes,
            block_number,
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

    async fn fetch_on_chain_guardian_binding(
        &self,
        account_id: &str,
        read_mode: RpcReadMode,
    ) -> Result<OnChainGuardianBinding, String> {
        let account_id = AccountId::from_hex(account_id).map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Invalid Miden account ID format in fetch_on_chain_guardian_binding"
            );
            format!("Invalid Miden account ID format: {e}")
        })?;

        // Storage details are only valid for public accounts; the node
        // holds a bare commitment for everything else, so there is
        // nothing to ask for.
        if !account_id.is_public() {
            return Ok(OnChainGuardianBinding::Opaque);
        }

        let rpc_started = std::time::Instant::now();
        let rpc_result = self
            .client
            .get_account_with_details(
                &account_id,
                Some(Self::guardian_map_detail_request()),
                read_mode,
            )
            .await;
        record_rpc("get_account_with_details", rpc_started, rpc_result.is_ok());

        // Only the release sweep reads storage details, and it defers and
        // retries a failed read, so this is a warning rather than an
        // error (the ERROR log alarm pages on node outages otherwise).
        let response = rpc_result.map_err(|e| {
            tracing::warn!(
                account_id = %account_id.to_hex(),
                error = %e,
                "Failed to fetch account storage details from Miden network"
            );
            format!(
                "Failed to read guardian binding for '{}' on Miden network: {e}",
                account_id.to_hex()
            )
        })?;

        Self::guardian_binding_from_response(&response)
    }

    async fn find_transaction_ending_at(
        &self,
        account_id: &str,
        final_state_commitment: &str,
        from_block: u32,
        read_mode: RpcReadMode,
    ) -> Result<TransactionSearch, String> {
        let account_id = AccountId::from_hex(account_id).map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Invalid Miden account ID format in find_transaction_ending_at"
            );
            format!("Invalid Miden account ID format: {e}")
        })?;

        // The node rejects a range that ends past its tip, so the search
        // is bounded by the tip read first.
        let rpc_started = std::time::Instant::now();
        let tip = self.client.get_chain_tip(read_mode).await;
        record_rpc("get_chain_tip", rpc_started, tip.is_ok());
        // Sweep-only reads, deferred and retried: warnings, see
        // `fetch_on_chain_guardian_binding`.
        let tip = tip.map_err(|e| {
            tracing::warn!(error = %e, "Failed to read the Miden chain tip");
            format!("Failed to read the chain tip on Miden network: {e}")
        })?;

        let mut from = from_block;
        while from <= tip {
            let rpc_started = std::time::Instant::now();
            let page = self
                .client
                .sync_transactions(&account_id, from, tip, read_mode)
                .await;
            record_rpc("sync_transactions", rpc_started, page.is_ok());
            let page = page.map_err(|e| {
                tracing::warn!(
                    account_id = %account_id.to_hex(),
                    error = %e,
                    "Failed to sync account transactions from Miden network"
                );
                format!(
                    "Failed to read transactions of '{}' on Miden network: {e}",
                    account_id.to_hex()
                )
            })?;
            let (found, covered_through) =
                Self::scan_transaction_page(&page, final_state_commitment)?;
            if found.is_some() {
                return Ok(TransactionSearch {
                    found_in_block: found,
                    resume_from_block: covered_through.saturating_add(1),
                });
            }
            // Every page must cover at least the block it started from
            // (anything else would loop forever), and never claim blocks
            // past the requested end (the watermark would skip blocks never
            // searched).
            if covered_through < from {
                return Err(format!(
                    "transaction page for '{}' made no progress past block {from}",
                    account_id.to_hex()
                ));
            }
            if covered_through > tip {
                return Err(format!(
                    "transaction page for '{}' claims block {covered_through}, past the \
                     requested end {tip}",
                    account_id.to_hex()
                ));
            }
            from = match covered_through.checked_add(1) {
                Some(next) => next,
                None => break,
            };
        }
        Ok(TransactionSearch {
            found_in_block: None,
            resume_from_block: from,
        })
    }

    fn account_nonce(&self, state_json: &serde_json::Value) -> Result<Option<u64>, String> {
        let account = Account::from_json(state_json)?;
        Ok(Some(account.nonce().as_canonical_u64()))
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

/// Outbound chain-node RPC metrics for one call (`operation` is the static
/// RPC method name, a closed set).
fn record_rpc(operation: &'static str, started: std::time::Instant, ok: bool) {
    metrics::counter!(
        crate::metrics::names::MIDEN_RPC_REQUESTS_TOTAL,
        crate::metrics::names::LABEL_OPERATION => operation,
        crate::metrics::names::LABEL_OUTCOME =>
            crate::metrics::labels::Outcome::from_ok(ok).as_str()
    )
    .increment(1);
    metrics::histogram!(
        crate::metrics::names::MIDEN_RPC_DURATION_SECONDS,
        crate::metrics::names::LABEL_OPERATION => operation
    )
    .record(started.elapsed().as_secs_f64());
}

/// Merges relative account deltas into one, replacing the upstream
/// `AccountDelta::merge` removed in Miden 0.16: storage patches merge
/// natively, vault deltas net once across all of them, and nonce deltas add.
/// The code of a later delta replaces an earlier one, as a code upgrade does.
fn merge_account_deltas<'a>(
    deltas: impl IntoIterator<Item = &'a AccountDelta>,
) -> Result<AccountDelta, String> {
    let mut deltas = deltas.into_iter();
    let first = deltas
        .next()
        .ok_or_else(|| "no account deltas to merge".to_string())?;
    let mut storage = first.storage().clone();
    let mut code = first.code().clone();
    let mut nonce_delta = first.nonce_delta();
    let mut vaults = vec![first.vault()];
    for delta in deltas {
        code.merge(delta.code().clone());
        storage
            .merge(delta.storage().clone())
            .map_err(|e| format!("failed to merge storage patches: {e}"))?;
        vaults.push(delta.vault());
        nonce_delta += delta.nonce_delta();
    }

    AccountDelta::new(
        first.id(),
        storage,
        merge_vault_deltas(vaults)?,
        code,
        nonce_delta,
    )
    .map_err(|e| format!("failed to build merged delta: {e}"))
}

/// Nets vault deltas into one. Since Miden 0.17 a vault delta is a set of whole
/// assets added or removed, one entry per asset, so fungible amounts net per
/// faucet and a non-fungible asset one delta adds and another removes cancels.
fn merge_vault_deltas<'a>(
    deltas: impl IntoIterator<Item = &'a AccountVaultDelta>,
) -> Result<AccountVaultDelta, String> {
    let mut fungible: BTreeMap<AccountId, i128> = BTreeMap::new();
    let mut non_fungible: BTreeMap<AssetId, AssetDelta> = BTreeMap::new();
    for asset_delta in deltas.into_iter().flat_map(AccountVaultDelta::iter) {
        let asset = asset_delta.asset();
        let sign: i128 = match asset_delta.delta_op() {
            AssetDeltaOperation::Add => 1,
            AssetDeltaOperation::Remove => -1,
        };
        match asset.as_fungible() {
            Some(fungible_asset) => {
                *fungible.entry(fungible_asset.faucet_id()).or_insert(0) +=
                    sign * i128::from(fungible_asset.amount().as_u64());
            }
            None => match non_fungible.remove(&asset.id()) {
                None => {
                    non_fungible.insert(asset.id(), *asset_delta);
                }
                Some(previous) if previous.delta_op() != asset_delta.delta_op() => {}
                Some(_) => {
                    return Err(format!(
                        "non-fungible asset {} is {:?}ed twice across merged deltas",
                        asset.faucet_id(),
                        asset_delta.delta_op()
                    ));
                }
            },
        }
    }

    let mut asset_deltas: Vec<AssetDelta> = non_fungible.into_values().collect();
    for (faucet_id, net) in fungible {
        let (operation, magnitude) = match net {
            0 => continue,
            net if net > 0 => (AssetDeltaOperation::Add, net),
            net => (AssetDeltaOperation::Remove, -net),
        };
        let amount = u64::try_from(magnitude)
            .map_err(|_| format!("merged fungible amount for {faucet_id} overflows u64"))?;
        let fungible_asset = FungibleAsset::new(faucet_id, amount)
            .map_err(|e| format!("failed to build merged fungible asset: {e}"))?;
        asset_deltas.push(AssetDelta::new(operation, Asset::from(fungible_asset)));
    }
    AccountVaultDelta::new(asset_deltas).map_err(|e| format!("failed to merge vault deltas: {e}"))
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use miden_protocol::Felt;
    use miden_protocol::transaction::TransactionSummaryUserParams;
    use miden_protocol::utils::serde::Serializable;

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

    // --- Guardian binding read (issue #434) --------------------------------

    const BLOCK: u32 = 77;
    const NONCE: u64 = 3;

    /// A word the way the node encodes it: its 32 canonical bytes.
    fn proto_word(word: &Word) -> primitives::Word {
        primitives::Word {
            encoded: word.as_bytes().to_vec(),
        }
    }

    fn word_hex(word: &Word) -> String {
        format!("0x{}", hex::encode(word.to_bytes()))
    }

    fn account_response(
        commitment: &Word,
        details: Option<rpc::get_account_response::AccountDetails>,
    ) -> rpc::GetAccountResponse {
        rpc::GetAccountResponse {
            block_num: Some(miden_rpc_client::blockchain::BlockNumber { block_num: BLOCK }),
            witness: Some(miden_rpc_client::account::AccountWitness {
                witness_id: None,
                commitment: Some(proto_word(commitment)),
                path: None,
            }),
            details,
        }
    }

    fn guardian_map_details(
        slot_name: &str,
        result: rpc::account_storage_details::account_storage_map_details::Result,
    ) -> rpc::get_account_response::AccountDetails {
        rpc::get_account_response::AccountDetails {
            header: Some(miden_rpc_client::account::AccountHeader {
                nonce: NONCE,
                ..Default::default()
            }),
            storage_details: Some(rpc::AccountStorageDetails {
                header: None,
                map_details: vec![rpc::account_storage_details::AccountStorageMapDetails {
                    slot_name: slot_name.to_string(),
                    result: Some(result),
                }],
            }),
            code: None,
            vault_details: None,
        }
    }

    fn raw_entries(
        entries: Vec<(Option<primitives::Word>, Option<primitives::Word>)>,
    ) -> rpc::account_storage_details::account_storage_map_details::Result {
        use rpc::account_storage_details::account_storage_map_details::all_map_entries::StorageMapEntry;
        rpc::account_storage_details::account_storage_map_details::Result::AllEntries(
            rpc::account_storage_details::account_storage_map_details::AllMapEntries {
                entries: entries
                    .into_iter()
                    .map(|(key, value)| StorageMapEntry { key, value })
                    .collect(),
            },
        )
    }

    fn all_entries(
        entries: &[(Word, Word)],
    ) -> rpc::account_storage_details::account_storage_map_details::Result {
        raw_entries(
            entries
                .iter()
                .map(|(key, value)| (Some(proto_word(key)), Some(proto_word(value))))
                .collect(),
        )
    }

    fn visible(commitment: &Word, guardian: Option<&Word>) -> OnChainGuardianBinding {
        OnChainGuardianBinding::Visible {
            on_chain_commitment: word_hex(commitment),
            guardian_commitment: guardian.map(word_hex),
            nonce: NONCE,
            block_num: BLOCK,
        }
    }

    #[test]
    fn node_words_hex_encode_like_local_words() {
        // The inspector encodes the guardian key with `Word::to_bytes()`
        // and state commitments use `Word::as_bytes()`; the node's word
        // bytes must map onto the same hex or the comparison is
        // meaningless.
        let word = Word::from([
            Felt::new_unchecked(1),
            Felt::new_unchecked(0xdead_beef),
            Felt::new_unchecked(u64::MAX - 1_000_000),
            Felt::new_unchecked(42),
        ]);
        let node_hex = MidenNetworkClient::word_hex(&proto_word(&word), "test").unwrap();
        assert_eq!(node_hex, word_hex(&word));
        assert_eq!(node_hex, format!("0x{}", hex::encode(word.as_bytes())));
    }

    #[test]
    fn guardian_binding_reads_key_zero_of_the_guardian_map() {
        let commitment = Word::from([Felt::new_unchecked(7); 4]);
        let guardian = Word::from([Felt::new_unchecked(9); 4]);
        let other = Word::from([Felt::new_unchecked(3); 4]);
        let response = account_response(
            &commitment,
            Some(guardian_map_details(
                guardian_public_key_slot_name(),
                all_entries(&[
                    (Word::from([Felt::new_unchecked(1); 4]), other),
                    (Word::default(), guardian),
                ]),
            )),
        );

        assert_eq!(
            MidenNetworkClient::guardian_binding_from_response(&response).unwrap(),
            visible(&commitment, Some(&guardian)),
            "the read carries the state's nonce and the block it was answered at"
        );
    }

    #[test]
    fn guardian_binding_without_details_is_opaque() {
        // A private account: the node answers with the witness only.
        let commitment = Word::from([Felt::new_unchecked(7); 4]);
        let response = account_response(&commitment, None);
        assert_eq!(
            MidenNetworkClient::guardian_binding_from_response(&response).unwrap(),
            OnChainGuardianBinding::Opaque
        );
    }

    #[test]
    fn guardian_binding_absent_slot_or_zero_key_means_no_binding() {
        let commitment = Word::from([Felt::new_unchecked(7); 4]);

        // Published storage without the guardian slot at all.
        let unrelated = account_response(
            &commitment,
            Some(guardian_map_details(
                "miden::standards::some::other::slot",
                all_entries(&[(Word::default(), Word::from([Felt::new_unchecked(9); 4]))]),
            )),
        );
        assert_eq!(
            MidenNetworkClient::guardian_binding_from_response(&unrelated).unwrap(),
            visible(&commitment, None)
        );

        // The slot exists but key zero holds the zero word (the same
        // "no binding" the inspector reports for client-supplied state).
        let zeroed = account_response(
            &commitment,
            Some(guardian_map_details(
                guardian_public_key_slot_name(),
                all_entries(&[(Word::default(), Word::default())]),
            )),
        );
        assert_eq!(
            MidenNetworkClient::guardian_binding_from_response(&zeroed).unwrap(),
            visible(&commitment, None)
        );

        // The slot exists but has no entries.
        let empty = account_response(
            &commitment,
            Some(guardian_map_details(
                guardian_public_key_slot_name(),
                all_entries(&[]),
            )),
        );
        assert_eq!(
            MidenNetworkClient::guardian_binding_from_response(&empty).unwrap(),
            visible(&commitment, None)
        );
    }

    #[test]
    fn a_requested_map_the_node_skipped_is_an_error_not_no_binding() {
        // The storage header lists the guardian slot, but the node's map
        // answers do not include it: nothing can be concluded.
        use miden_rpc_client::account::account_storage_header::StorageSlot;
        let commitment = Word::from([Felt::new_unchecked(7); 4]);
        let mut details =
            guardian_map_details("miden::standards::some::other::slot", all_entries(&[]));
        details.storage_details.as_mut().unwrap().header =
            Some(miden_rpc_client::account::AccountStorageHeader {
                slots: vec![StorageSlot {
                    slot_name: guardian_public_key_slot_name().to_string(),
                    content: None,
                }],
            });
        let response = account_response(&commitment, Some(details));
        assert!(MidenNetworkClient::guardian_binding_from_response(&response).is_err());
    }

    #[test]
    fn an_empty_proto_word_is_never_read_as_key_zero() {
        // proto3's default `Word` is empty bytes, not the zero word. A
        // parser comparing keys against `primitives::Word::default()`
        // would never find the node's 32-zero-byte key, and every public
        // account would silently read as "no binding". An empty key is a
        // malformed answer instead.
        let commitment = Word::from([Felt::new_unchecked(7); 4]);
        let guardian = Word::from([Felt::new_unchecked(9); 4]);
        let response = account_response(
            &commitment,
            Some(guardian_map_details(
                guardian_public_key_slot_name(),
                raw_entries(vec![(
                    Some(primitives::Word::default()),
                    Some(proto_word(&guardian)),
                )]),
            )),
        );
        assert!(MidenNetworkClient::guardian_binding_from_response(&response).is_err());
        assert!(!MidenNetworkClient::is_zero_word(
            &primitives::Word::default()
        ));
        assert!(MidenNetworkClient::is_zero_word(&proto_word(
            &Word::default()
        )));
    }

    #[test]
    fn guardian_binding_rejects_unusable_map_answers_and_missing_witness() {
        use rpc::account_storage_details::account_storage_map_details::Result as MapResult;
        let commitment = Word::from([Felt::new_unchecked(7); 4]);
        let guardian = Word::from([Felt::new_unchecked(9); 4]);

        let too_many = account_response(
            &commitment,
            Some(guardian_map_details(
                guardian_public_key_slot_name(),
                MapResult::TooManyEntries(true),
            )),
        );
        assert!(MidenNetworkClient::guardian_binding_from_response(&too_many).is_err());

        let partial = account_response(
            &commitment,
            Some(guardian_map_details(
                guardian_public_key_slot_name(),
                MapResult::PartialMap(
                    rpc::account_storage_details::account_storage_map_details::PartialStorageMap {
                        map_keys: vec![],
                        partial_smt: None,
                    },
                ),
            )),
        );
        assert!(MidenNetworkClient::guardian_binding_from_response(&partial).is_err());

        let no_witness = rpc::GetAccountResponse {
            block_num: None,
            witness: None,
            details: None,
        };
        assert!(MidenNetworkClient::guardian_binding_from_response(&no_witness).is_err());

        // A witness commitment that is not a 32-byte word.
        let mut short_witness = account_response(&commitment, None);
        short_witness
            .witness
            .as_mut()
            .unwrap()
            .commitment
            .as_mut()
            .unwrap()
            .encoded
            .pop();
        assert!(MidenNetworkClient::guardian_binding_from_response(&short_witness).is_err());

        // Details without a storage answer: the node did not answer the
        // storage request, which must not read as "no binding".
        let mut no_storage = guardian_map_details(
            guardian_public_key_slot_name(),
            MapResult::TooManyEntries(true),
        );
        no_storage.storage_details = None;
        let no_storage = account_response(&commitment, Some(no_storage));
        assert!(MidenNetworkClient::guardian_binding_from_response(&no_storage).is_err());

        // Details without the account header: no nonce to order the read.
        let mut no_header = guardian_map_details(
            guardian_public_key_slot_name(),
            all_entries(&[(Word::default(), guardian)]),
        );
        no_header.header = None;
        let no_header = account_response(&commitment, Some(no_header));
        assert!(MidenNetworkClient::guardian_binding_from_response(&no_header).is_err());

        // A storage answer without the block it was observed at.
        let mut no_block = account_response(
            &commitment,
            Some(guardian_map_details(
                guardian_public_key_slot_name(),
                all_entries(&[(Word::default(), guardian)]),
            )),
        );
        no_block.block_num = None;
        assert!(MidenNetworkClient::guardian_binding_from_response(&no_block).is_err());

        // The guardian slot is listed but carries no result.
        let mut no_result = guardian_map_details(
            guardian_public_key_slot_name(),
            MapResult::TooManyEntries(true),
        );
        no_result.storage_details.as_mut().unwrap().map_details[0].result = None;
        let no_result = account_response(&commitment, Some(no_result));
        assert!(MidenNetworkClient::guardian_binding_from_response(&no_result).is_err());

        // A key-zero entry without a value is malformed, not "no binding".
        let valueless = account_response(
            &commitment,
            Some(guardian_map_details(
                guardian_public_key_slot_name(),
                raw_entries(vec![(Some(proto_word(&Word::default())), None)]),
            )),
        );
        assert!(MidenNetworkClient::guardian_binding_from_response(&valueless).is_err());

        // So are a keyless entry and a value that is not a 32-byte word.
        let keyless = account_response(
            &commitment,
            Some(guardian_map_details(
                guardian_public_key_slot_name(),
                raw_entries(vec![(None, Some(proto_word(&guardian)))]),
            )),
        );
        assert!(MidenNetworkClient::guardian_binding_from_response(&keyless).is_err());
        let short_value = account_response(
            &commitment,
            Some(guardian_map_details(
                guardian_public_key_slot_name(),
                raw_entries(vec![(
                    Some(proto_word(&Word::default())),
                    Some(primitives::Word {
                        encoded: vec![9; 31],
                    }),
                )]),
            )),
        );
        assert!(MidenNetworkClient::guardian_binding_from_response(&short_value).is_err());
    }

    // --- Transaction history search (issue #434) ---------------------------

    fn transaction_record(
        block_num: u32,
        initial: &Word,
        final_commitment: &Word,
    ) -> rpc::TransactionRecord {
        rpc::TransactionRecord {
            block_num,
            header: Some(miden_rpc_client::transaction::TransactionHeader {
                transaction_id: None,
                account_id: None,
                initial_state_commitment: Some(proto_word(initial)),
                final_state_commitment: Some(proto_word(final_commitment)),
                input_notes: vec![],
                output_notes: vec![],
            }),
            output_note_proofs: vec![],
            consumed_note_refs: vec![],
        }
    }

    fn transaction_page(
        covered_through: u32,
        chain_tip: u32,
        transactions: Vec<rpc::TransactionRecord>,
    ) -> rpc::SyncTransactionsResponse {
        rpc::SyncTransactionsResponse {
            pagination_info: Some(rpc::PaginationInfo {
                chain_tip,
                block_num: covered_through,
            }),
            transactions,
        }
    }

    #[test]
    fn a_page_matches_on_the_final_commitment_only() {
        let post_switch = Word::from([Felt::new_unchecked(5); 4]);
        let later = Word::from([Felt::new_unchecked(6); 4]);
        // A new account's first transaction records an EMPTY initial
        // commitment, not the state it was registered with; it must
        // still match on its final commitment.
        let page = transaction_page(
            90,
            100,
            vec![
                transaction_record(40, &Word::default(), &post_switch),
                transaction_record(41, &post_switch, &later),
            ],
        );
        assert_eq!(
            MidenNetworkClient::scan_transaction_page(&page, &word_hex(&post_switch)).unwrap(),
            (Some(40), 90)
        );
        assert_eq!(
            MidenNetworkClient::scan_transaction_page(
                &page,
                &word_hex(&post_switch).to_uppercase().replace("0X", "0x")
            )
            .unwrap(),
            (Some(40), 90),
            "hex case never hides a match"
        );
        assert_eq!(
            MidenNetworkClient::scan_transaction_page(
                &page,
                &word_hex(&Word::from([Felt::new_unchecked(8); 4]))
            )
            .unwrap(),
            (None, 90),
            "a miss still reports how far the page reached"
        );
    }

    #[test]
    fn malformed_transaction_pages_are_errors() {
        let target = word_hex(&Word::from([Felt::new_unchecked(5); 4]));
        let no_pagination = rpc::SyncTransactionsResponse {
            pagination_info: None,
            transactions: vec![],
        };
        assert!(MidenNetworkClient::scan_transaction_page(&no_pagination, &target).is_err());

        let mut headerless = transaction_page(
            10,
            10,
            vec![transaction_record(
                3,
                &Word::default(),
                &Word::from([Felt::new_unchecked(1); 4]),
            )],
        );
        headerless.transactions[0].header = None;
        assert!(MidenNetworkClient::scan_transaction_page(&headerless, &target).is_err());

        let mut short_final = transaction_page(
            10,
            10,
            vec![transaction_record(
                3,
                &Word::default(),
                &Word::from([Felt::new_unchecked(1); 4]),
            )],
        );
        short_final.transactions[0]
            .header
            .as_mut()
            .unwrap()
            .final_state_commitment = Some(primitives::Word { encoded: vec![] });
        assert!(MidenNetworkClient::scan_transaction_page(&short_final, &target).is_err());
    }

    async fn history_client(
        tip: u32,
        pages: Vec<rpc::SyncTransactionsResponse>,
    ) -> (
        MidenNetworkClient,
        std::sync::Arc<std::sync::Mutex<Vec<rpc::SyncTransactionsRequest>>>,
    ) {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let node = miden_rpc_client::test_node::ScriptedNode::failing(
            0,
            || tonic::Status::unavailable("unused"),
            std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        )
        .with_chain_tip(tip, pages, requests.clone());
        let endpoint = miden_rpc_client::test_node::serve(node).await;
        let client = MidenRpcClient::connect(endpoint)
            .await
            .expect("the scripted node accepts connections");
        (MidenNetworkClient { client }, requests)
    }

    fn history_account_id() -> String {
        "0xe33cde61b266e9c1753136d03a5572".to_string()
    }

    fn requested_ranges(
        requests: &std::sync::Mutex<Vec<rpc::SyncTransactionsRequest>>,
    ) -> Vec<(u32, u32)> {
        requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                let range = request.block_range.expect("range");
                (range.block_from, range.block_to)
            })
            .collect()
    }

    #[tokio::test]
    async fn the_history_search_pages_to_the_tip_and_finds_the_switch() {
        // The node caps pages by payload: the first covers blocks 0..=60,
        // the search continues from 61 and finds the switch in block 75
        // even though the account moved on afterwards.
        let post_switch = Word::from([Felt::new_unchecked(5); 4]);
        let (client, requests) = history_client(
            100,
            vec![
                transaction_page(
                    60,
                    100,
                    vec![transaction_record(
                        12,
                        &Word::default(),
                        &Word::from([Felt::new_unchecked(1); 4]),
                    )],
                ),
                transaction_page(
                    100,
                    100,
                    vec![
                        transaction_record(
                            75,
                            &Word::from([Felt::new_unchecked(1); 4]),
                            &post_switch,
                        ),
                        transaction_record(
                            90,
                            &post_switch,
                            &Word::from([Felt::new_unchecked(2); 4]),
                        ),
                    ],
                ),
            ],
        )
        .await;

        let search = client
            .find_transaction_ending_at(
                &history_account_id(),
                &word_hex(&post_switch),
                0,
                RpcReadMode::SingleAttempt,
            )
            .await
            .expect("search");
        assert_eq!(
            search,
            TransactionSearch {
                found_in_block: Some(75),
                resume_from_block: 101,
            }
        );
        assert_eq!(requested_ranges(&requests), vec![(0, 100), (61, 100)]);
    }

    #[tokio::test]
    async fn a_missed_search_resumes_after_the_tip_and_skips_covered_blocks() {
        let target = word_hex(&Word::from([Felt::new_unchecked(5); 4]));
        let (client, requests) =
            history_client(100, vec![transaction_page(100, 100, vec![])]).await;

        let first = client
            .find_transaction_ending_at(
                &history_account_id(),
                &target,
                40,
                RpcReadMode::SingleAttempt,
            )
            .await
            .expect("search");
        assert_eq!(
            first,
            TransactionSearch {
                found_in_block: None,
                resume_from_block: 101,
            }
        );

        // Resuming past the tip reads nothing but the tip.
        let second = client
            .find_transaction_ending_at(
                &history_account_id(),
                &target,
                101,
                RpcReadMode::SingleAttempt,
            )
            .await
            .expect("search");
        assert_eq!(second, first);
        assert_eq!(requested_ranges(&requests), vec![(40, 100)]);
    }

    #[tokio::test]
    async fn a_page_that_makes_no_progress_is_an_error_not_a_loop() {
        let target = word_hex(&Word::from([Felt::new_unchecked(5); 4]));
        let (client, _) = history_client(100, vec![transaction_page(9, 100, vec![])]).await;
        let error = client
            .find_transaction_ending_at(
                &history_account_id(),
                &target,
                10,
                RpcReadMode::SingleAttempt,
            )
            .await
            .expect_err("no progress");
        assert!(error.contains("no progress"), "{error}");
    }

    #[tokio::test]
    async fn a_page_claiming_blocks_past_the_requested_end_is_an_error() {
        // Trusting it would move the resume watermark past blocks that
        // were never searched.
        let target = word_hex(&Word::from([Felt::new_unchecked(5); 4]));
        let (client, _) = history_client(100, vec![transaction_page(150, 150, vec![])]).await;
        let error = client
            .find_transaction_ending_at(
                &history_account_id(),
                &target,
                10,
                RpcReadMode::SingleAttempt,
            )
            .await
            .expect_err("a page past the requested end");
        assert!(error.contains("past the requested end"), "{error}");
    }

    #[tokio::test]
    async fn a_page_above_the_default_grpc_limit_is_still_read() {
        // The node fills a page up to 4 MiB by its own size estimate, which
        // undercounts the encoded size: a full page can exceed tonic's
        // 4 MiB default and must still decode.
        use prost::Message;
        let filler = Word::from([Felt::new_unchecked(1); 4]);
        let post_switch = Word::from([Felt::new_unchecked(5); 4]);
        let record = transaction_record(10, &filler, &filler);
        let per_record = record.encoded_len() + 4;
        let records = (4 * 1024 * 1024 + 256 * 1024) / per_record;
        let mut page = transaction_page(100, 100, vec![record; records]);
        page.transactions
            .push(transaction_record(75, &filler, &post_switch));
        let size = page.encoded_len();
        assert!(size > 4 * 1024 * 1024, "{size}");
        assert!(size < miden_rpc_client::MAX_RESPONSE_SIZE_BYTES, "{size}");

        let (client, _) = history_client(100, vec![page]).await;
        let search = client
            .find_transaction_ending_at(
                &history_account_id(),
                &word_hex(&post_switch),
                0,
                RpcReadMode::SingleAttempt,
            )
            .await
            .expect("a page above 4 MiB decodes");
        assert_eq!(search.found_in_block, Some(75));
    }

    #[tokio::test]
    async fn account_nonce_reads_the_stored_state() {
        let client = MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal);
        let account_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse account fixture");
        let account = Account::from_json(&account_json).expect("fixture account");
        assert_eq!(
            client.account_nonce(&account_json).unwrap(),
            Some(account.nonce().as_canonical_u64())
        );
        assert!(client.account_nonce(&serde_json::json!({})).is_err());
    }

    #[test]
    fn guardian_map_request_asks_for_the_guardian_slot_only() {
        use rpc::get_account_request::account_detail_request::StorageRequest;
        use rpc::get_account_request::account_detail_request::storage_map_detail_request::SlotData;
        let request = MidenNetworkClient::guardian_map_detail_request();
        assert!(request.code_commitment.is_none());
        assert!(request.asset_vault_commitment.is_none());
        let Some(StorageRequest::StorageMaps(maps)) = request.storage_request else {
            panic!("expected an explicit storage-map selection");
        };
        assert_eq!(maps.storage_maps.len(), 1);
        assert_eq!(
            maps.storage_maps[0].slot_name,
            guardian_public_key_slot_name()
        );
        assert_eq!(
            maps.storage_maps[0].slot_data,
            Some(SlotData::AllEntries(true))
        );
    }

    #[tokio::test]
    async fn private_account_id_short_circuits_to_opaque_without_an_rpc() {
        // `lazy_for_test` never connects, so any RPC attempt would fail:
        // an `Ok(Opaque)` proves the private-account branch returned
        // before touching the network.
        use miden_protocol::account::{AccountIdVersion, AccountType, AssetCallbackFlag};
        let client = MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal);
        let private_id = AccountId::dummy(
            [1u8; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            AssetCallbackFlag::Disabled,
        );
        assert!(private_id.is_private());
        let binding = client
            .fetch_on_chain_guardian_binding(&private_id.to_hex(), RpcReadMode::SingleAttempt)
            .await
            .expect("private accounts never issue the storage read");
        assert_eq!(binding, OnChainGuardianBinding::Opaque);
    }

    #[tokio::test]
    #[ignore = "requires live network access and system TLS roots; covered by integration suites"]
    async fn test_client_from_network_type() {
        let network = NetworkType::MidenTestnet;
        let result = MidenNetworkClient::from_network(network).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_state_head_invalid_state_json() {
        let network = NetworkType::MidenTestnet;
        let client = MidenNetworkClient::lazy_for_test(network);

        let account_id_hex = "0x8a8a8a8a8a8a8a010a8a8a8a8a8a8a";
        let state_json = serde_json::json!({"balance": 0});

        let result = client.get_state_head(account_id_hex, &state_json);
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
    async fn test_get_state_head_invalid_format() {
        let network = NetworkType::MidenTestnet;
        let client = MidenNetworkClient::lazy_for_test(network);

        let invalid_account_id = "not_a_valid_hex";
        let state_json = serde_json::json!({"balance": 0});

        let result = client.get_state_head(invalid_account_id, &state_json);
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

        let applied = client
            .apply_delta(&account_json, delta_payload)
            .expect("apply_delta should succeed");

        assert_eq!(
            applied.commitment, expected_commitment,
            "Commitment after apply_delta should match expected"
        );

        assert!(
            applied.state_json.get("data").is_some(),
            "New state should have data field"
        );
    }

    #[tokio::test]
    async fn account_nonce_tracks_applied_deltas() {
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

        let before = client
            .account_nonce(&account_json)
            .expect("account_nonce should succeed on the fixture")
            .expect("a Miden account always carries a nonce");

        let applied = client
            .apply_delta(&account_json, delta_payload)
            .expect("apply_delta should succeed");
        let after = client
            .account_nonce(&applied.state_json)
            .expect("account_nonce should succeed after apply_delta")
            .expect("a Miden account always carries a nonce");
        assert!(
            after > before,
            "applying a delta must advance the nonce ({before} -> {after})"
        );
    }

    /// The nonce stored with a state (issue #191) comes from the head the
    /// write path already decoded: `get_state_head` on configure and
    /// `apply_delta` on the optimistic commit and canonicalization. It
    /// must equal the nonce a later decode of the stored blob reads, or
    /// the canonical-nonce pre-check would serve a different nonce than
    /// `GET /state` holds.
    #[tokio::test]
    async fn stored_heads_carry_the_decoded_nonce() {
        let client = MidenNetworkClient::lazy_for_test(NetworkType::MidenTestnet);
        let account_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse account fixture");
        let delta_fixture: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::DELTA_1_JSON)
                .expect("Failed to parse delta fixture");
        let delta_payload = delta_fixture
            .get("delta_payload")
            .expect("delta_payload field missing");
        let account_id = Account::from_json(&account_json)
            .expect("fixture account")
            .id()
            .to_hex();

        // Configure: the head of the submitted initial state.
        let configured = client
            .get_state_head(&account_id, &account_json)
            .expect("get_state_head should succeed on the fixture");
        assert_eq!(
            configured.nonce,
            client.account_nonce(&account_json).unwrap(),
            "configure stores the nonce a decode of the configured state reads"
        );
        assert!(configured.nonce.is_some(), "a Miden head carries a nonce");

        // Optimistic commit and canonicalization: the state apply_delta built.
        let applied = client
            .apply_delta(&account_json, delta_payload)
            .expect("apply_delta should succeed");
        assert_eq!(
            applied.nonce,
            client.account_nonce(&applied.state_json).unwrap(),
            "apply_delta returns the nonce a decode of the new state reads"
        );
        assert_eq!(
            client
                .get_state_head(&account_id, &applied.state_json)
                .expect("the applied state decodes"),
            StateHead {
                commitment: applied.commitment.clone(),
                nonce: applied.nonce,
            },
            "apply_delta's commitment and nonce describe the state it returns"
        );
        assert!(
            applied.nonce > configured.nonce,
            "the applied state is past the configured one"
        );
    }

    /// The push path and promotion read the post-delta auth bindings off
    /// `AppliedState` instead of decoding the stored blob again (issue
    /// #328), so they must equal what a fresh decode of that blob reads.
    #[tokio::test]
    async fn verify_and_apply_delta_matches_separate_passes() {
        let client = MidenNetworkClient::lazy_for_test(NetworkType::MidenTestnet);
        let account_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse account fixture");
        let delta_fixture: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::DELTA_1_JSON)
                .expect("Failed to parse delta fixture");
        let delta_payload = delta_fixture
            .get("delta_payload")
            .expect("delta_payload field missing");
        let prev_commitment = format!(
            "0x{}",
            hex::encode(
                Account::from_json(&account_json)
                    .expect("fixture account")
                    .to_commitment()
                    .as_bytes()
            )
        );

        let applied = client
            .verify_and_apply_delta(&prev_commitment, &account_json, delta_payload)
            .expect("verify_and_apply_delta should succeed");
        assert_eq!(
            applied,
            client
                .apply_delta(&account_json, delta_payload)
                .expect("apply_delta should succeed"),
            "the single-decode path must produce the state the separate passes produce"
        );

        assert!(
            client
                .verify_and_apply_delta("0xnot_the_base", &account_json, delta_payload)
                .expect_err("a wrong base commitment must be rejected")
                .contains("Previous commitment mismatch"),
        );

        let current_auth = Auth::MidenFalconRpo {
            cosigner_commitments: Vec::new(),
        };
        assert!(!applied.cosigner_commitments.is_empty());
        assert_eq!(
            applied.updated_auth(&current_auth),
            client
                .should_update_auth(&applied.state_json, &current_auth)
                .await
                .expect("should_update_auth should succeed"),
        );
        assert!(applied.guardian_commitment.is_some());
        assert_eq!(
            applied.guardian_commitment,
            client
                .extract_guardian_commitment(&applied.state_json)
                .expect("extract_guardian_commitment should succeed"),
        );
    }

    #[tokio::test]
    async fn test_apply_delta_full_state() {
        use miden_protocol::Felt;
        use miden_protocol::account::delta::AccountVaultDelta;
        use miden_protocol::account::{AccountBuilder, AccountType};
        use miden_protocol::account::{AccountCodePatch, AccountDelta};
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
            AccountCodePatch::new(Some(account.code().clone())),
            Felt::new_unchecked(1),
        )
        .expect("Failed to create delta");

        assert!(
            !full_state_delta.code().is_empty(),
            "Delta should carry the account code"
        );

        // Create a TransactionSummary with the full state delta
        let tx_summary = TransactionSummary::new(
            full_state_delta,
            InputNotes::new(Vec::new()).expect("empty input notes"),
            RawOutputNotes::new(Vec::new()).expect("empty output notes"),
            miden_protocol::block::BlockNumber::from(0),
            Word::default(),
            0,
            TransactionSummaryUserParams::new([Felt::ZERO; 6]),
        );

        let delta_payload = tx_summary.to_json();

        // Full-state deltas read prev_state_json only for the pre-tx guardian
        // selector; an unparseable prev state falls back to guardian-disabled
        // (with a warn), which is fine here — this account has no guardian
        // component, so the empty prev state exercises exactly that fallback.
        let empty_prev_state = serde_json::json!({});

        let applied = client
            .apply_delta(&empty_prev_state, &delta_payload)
            .expect("apply_delta with full state should succeed");

        // The new state should have a data field
        assert!(
            applied.state_json.get("data").is_some(),
            "New state from full delta should have data field"
        );

        // Commitment should be a valid hex string
        assert!(
            applied.commitment.starts_with("0x"),
            "Commitment should be hex format"
        );
        assert_eq!(
            applied.commitment.len(),
            66,
            "Commitment should be 32 bytes (64 hex chars + 0x prefix)"
        );

        // A new account's first state carries the delta's final nonce, and
        // it is the nonce a decode of that state reads.
        assert_eq!(applied.nonce, Some(1));
        assert_eq!(
            client.account_nonce(&applied.state_json).unwrap(),
            applied.nonce
        );
    }
}
