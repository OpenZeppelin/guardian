//! Internal helper functions for GUARDIAN client interactions.

use crate::guardian_endpoint::verify_endpoint_commitment;
use guardian_client::GuardianClient;
#[cfg(test)]
use guardian_shared::FromJson;
use guardian_shared::SignatureScheme;
use guardian_shared::ToJson;
use miden_client::account::Account;
use miden_client::rpc::{GrpcClient, GrpcError, NodeRpcClient, RpcError};
use miden_client::transaction::{TransactionRequest, TransactionSummary};
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::utils::serde::Serializable;

use super::MultisigClient;
use crate::account::MultisigAccount;
use crate::builder::create_miden_client;
use crate::error::{MultisigError, Result};
use crate::execution::build_final_transaction_request;
use crate::keystore::word_from_hex;
use crate::proposal::{Proposal, TransactionType};
use crate::transaction::word_to_hex;

impl MultisigClient {
    /// Creates a GUARDIAN client (unauthenticated).
    pub(crate) async fn create_guardian_client(&self) -> Result<GuardianClient> {
        GuardianClient::connect(&self.guardian_endpoint)
            .await
            .map_err(|e| MultisigError::GuardianConnection(e.to_string()))
    }

    /// Creates an authenticated GUARDIAN client.
    pub(crate) async fn create_authenticated_guardian_client(&self) -> Result<GuardianClient> {
        let client = self.create_guardian_client().await?;
        Ok(client.with_signer(self.key_manager.clone()))
    }

    pub(crate) async fn get_on_chain_account_commitment(
        &self,
        account_id: AccountId,
    ) -> Result<Word> {
        let rpc_client = GrpcClient::new(&self.miden_endpoint, 10_000);
        let fetched_account = rpc_client
            .get_account_details(account_id)
            .await
            .map_err(|e| {
                MultisigError::MidenClient(format!(
                    "failed to fetch on-chain commitment for account {}: {}",
                    account_id, e
                ))
            })?;

        Ok(fetched_account.commitment())
    }

    pub(crate) async fn try_get_on_chain_account_commitment(
        &self,
        account_id: AccountId,
    ) -> Result<Option<Word>> {
        let rpc_client = GrpcClient::new(&self.miden_endpoint, 10_000);
        match rpc_client.get_account_details(account_id).await {
            Ok(fetched_account) => {
                let commitment = fetched_account.commitment();
                if commitment == Word::default() {
                    Ok(None)
                } else {
                    Ok(Some(commitment))
                }
            }
            Err(RpcError::RequestError {
                error_kind: GrpcError::NotFound,
                ..
            }) => Ok(None),
            Err(e) => Err(MultisigError::MidenClient(format!(
                "failed to fetch on-chain commitment for account {}: {}",
                account_id, e
            ))),
        }
    }

    /// Returns a reference to the current account, or error if none loaded.
    pub(crate) fn require_account(&self) -> Result<&MultisigAccount> {
        self.account
            .as_ref()
            .ok_or_else(|| MultisigError::MissingConfig("no account loaded".to_string()))
    }

    pub(crate) fn ensure_proposal_account_id(
        proposal_account_id: &str,
        expected_account_id: &AccountId,
    ) -> Result<()> {
        if proposal_account_id.eq_ignore_ascii_case(&expected_account_id.to_string()) {
            return Ok(());
        }

        Err(MultisigError::InvalidConfig(format!(
            "proposal is for account {} instead of {}",
            proposal_account_id, expected_account_id
        )))
    }

    /// Gets the GUARDIAN acknowledgment signature for a transaction.
    ///
    /// This pushes the delta to GUARDIAN and retrieves the server's signature.
    pub(crate) async fn get_guardian_ack_signature(
        &mut self,
        account: &MultisigAccount,
        nonce: u64,
        tx_summary: &TransactionSummary,
        tx_summary_commitment: Word,
    ) -> Result<crate::execution::SignatureAdvice> {
        let account_id = account.id();
        let prev_commitment = format!(
            "0x{}",
            hex::encode(Serializable::to_bytes(&account.commitment()))
        );

        // Push delta to GUARDIAN to get acknowledgment signature
        let mut guardian_client = self.create_authenticated_guardian_client().await?;
        let delta_payload = tx_summary.to_json();

        let push_response = guardian_client
            .push_delta(&account_id, nonce, &prev_commitment, &delta_payload)
            .await
            .map_err(|e| MultisigError::GuardianServer(format!("failed to push delta: {}", e)))?;

        // Get GUARDIAN ack signature
        let ack_sig = push_response.ack_sig.ok_or_else(|| {
            MultisigError::GuardianServer(
                "GUARDIAN did not return acknowledgment signature".to_string(),
            )
        })?;
        let ack_scheme = push_response
            .delta
            .as_ref()
            .and_then(|delta| delta.ack_scheme.as_deref())
            .ok_or_else(|| {
                MultisigError::GuardianServer(
                    "GUARDIAN did not return acknowledgment scheme".to_string(),
                )
            })
            .and_then(|ack_scheme| {
                SignatureScheme::from(ack_scheme).map_err(MultisigError::GuardianServer)
            })?;

        let (guardian_commitment_hex, raw_pubkey) = guardian_client
            .get_pubkey(Some(ack_scheme.as_str()))
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!("failed to get GUARDIAN commitment: {}", e))
            })?;

        let guardian_commitment =
            word_from_hex(&guardian_commitment_hex).map_err(MultisigError::HexDecode)?;
        let expected_guardian_commitment = account.guardian_commitment()?;
        if guardian_commitment != expected_guardian_commitment {
            return Err(MultisigError::GuardianServer(format!(
                "GUARDIAN public key commitment {} does not match account commitment {}",
                word_to_hex(&guardian_commitment),
                word_to_hex(&expected_guardian_commitment)
            )));
        }

        let ack_signature = ack_scheme
            .parse_signature_hex(&ack_sig)
            .map_err(MultisigError::Signature)?;
        ack_scheme
            .build_signature_advice_entry(
                guardian_commitment,
                tx_summary_commitment,
                &ack_signature,
                push_response
                    .delta
                    .as_ref()
                    .and_then(|delta| delta.ack_pubkey.as_deref())
                    .or(raw_pubkey.as_deref()),
            )
            .map_err(MultisigError::Signature)
    }

    /// Verifies that a proposals metadata reconstructs the same tx_summary commitment.
    pub(crate) async fn verify_proposal_summary_binding(
        &mut self,
        proposal: &Proposal,
    ) -> Result<()> {
        let tx_summary_commitment = proposal.tx_summary.to_commitment();

        let proposal_id_commitment = word_to_hex(&tx_summary_commitment);
        if !proposal.id.eq_ignore_ascii_case(&proposal_id_commitment) {
            return Err(MultisigError::InvalidConfig(format!(
                "proposal id {} does not match tx_summary commitment {}",
                proposal.id, proposal_id_commitment
            )));
        }

        let account = self.require_account()?.clone();
        let salt = proposal.metadata.salt()?;
        let signer_commitments = proposal.metadata.signer_commitments()?;

        let tx_request = build_final_transaction_request(
            &self.miden_client,
            &proposal.transaction_type,
            account.inner(),
            salt,
            Vec::new(),
            proposal.metadata.new_threshold,
            Some(signer_commitments.as_slice()),
            self.key_manager.scheme(),
        )
        .await?;

        let reconstructed = crate::transaction::execute_for_summary(
            &mut self.miden_client,
            account.id(),
            tx_request,
        )
        .await?;

        if reconstructed.to_commitment() != tx_summary_commitment {
            return Err(MultisigError::InvalidConfig(format!(
                "proposal {} metadata does not match tx_summary",
                proposal.id
            )));
        }

        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn proposal_id_from_delta_payload(delta_payload: &str) -> Result<String> {
        let payload_json: serde_json::Value = serde_json::from_str(delta_payload).map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to parse proposal delta payload: {}", e))
        })?;
        let tx_summary_json = payload_json.get("tx_summary").ok_or_else(|| {
            MultisigError::InvalidConfig("missing tx_summary in delta payload".to_string())
        })?;
        let tx_summary = TransactionSummary::from_json(tx_summary_json).map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to parse tx_summary: {}", e))
        })?;
        Ok(word_to_hex(&tx_summary.to_commitment()))
    }

    /// Finalizes a transaction by executing it on-chain and updating local state.
    ///
    /// This handles the common post-execution logic for all proposal types.
    pub(crate) async fn finalize_transaction(
        &mut self,
        account_id: AccountId,
        tx_request: TransactionRequest,
        transaction_type: &TransactionType,
    ) -> Result<()> {
        if let TransactionType::SwitchGuardian {
            new_endpoint,
            new_commitment,
        } = transaction_type
        {
            verify_endpoint_commitment(new_endpoint, *new_commitment).await?;
        }

        // Capture the new GUARDIAN endpoint if this is a SwitchGuardian transaction
        let new_guardian_endpoint =
            if let TransactionType::SwitchGuardian { new_endpoint, .. } = transaction_type {
                Some(new_endpoint.clone())
            } else {
                None
            };

        // Execute the transaction on-chain
        self.miden_client
            .submit_new_transaction(account_id, tx_request)
            .await
            .map_err(|e| {
                MultisigError::TransactionExecution(format!(
                    "transaction execution failed: {:?}",
                    e
                ))
            })?;

        // Try to sync with the network to ensure consistent state.
        if let Err(_e) = self.miden_client.sync_state().await {
            // Intentionally ignored, GUARDIAN may not have canonicalized yet.
        }

        // Get updated account from miden-client's local state
        let account_record = self
            .miden_client
            .get_account(account_id)
            .await
            .map_err(|e| {
                MultisigError::MidenClient(format!("failed to get updated account: {}", e))
            })?
            .ok_or_else(|| {
                MultisigError::MissingConfig("account not found after execution".to_string())
            })?;

        let updated_account: Account = account_record.try_into().map_err(|e| {
            MultisigError::MidenClient(format!("account record is not full: {}", e))
        })?;

        // Update GUARDIAN endpoint if this was a SwitchGuardian transaction, then register on new GUARDIAN
        if let Some(endpoint) = new_guardian_endpoint {
            self.guardian_endpoint = endpoint;

            // Refresh the local account after switching to the new GUARDIAN endpoint.
            let multisig_account = MultisigAccount::new(updated_account.clone());
            self.account = Some(multisig_account);

            // Register the updated account on the new GUARDIAN server
            self.register_on_guardian().await.map_err(|e| {
                MultisigError::GuardianServer(format!(
                    "transaction executed successfully but failed to register on new GUARDIAN: {}",
                    e
                ))
            })?;
        } else {
            let multisig_account = MultisigAccount::new(updated_account);
            self.account = Some(multisig_account);
        }

        Ok(())
    }

    /// Resets the miden-client by creating a new instance with a fresh database.
    pub async fn reset_miden_client(&mut self) -> Result<()> {
        self.miden_client = create_miden_client(&self.account_dir, &self.miden_endpoint).await?;
        Ok(())
    }

    /// Adds an account to miden-client if it doesn't exist, or updates it if it does.
    pub(crate) async fn add_or_update_account(
        &mut self,
        account: &Account,
        imported: bool,
    ) -> Result<()> {
        let account_id = account.id();

        let existing = self
            .miden_client
            .get_account(account_id)
            .await
            .map_err(|e| MultisigError::MidenClient(format!("failed to check account: {}", e)))?;

        if existing.is_some() {
            self.miden_client
                .add_account(account, true)
                .await
                .map_err(|e| {
                    MultisigError::MidenClient(format!("failed to update account: {}", e))
                })?;
        } else {
            self.miden_client
                .add_account(account, imported)
                .await
                .map_err(|e| MultisigError::MidenClient(format!("failed to add account: {}", e)))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use guardian_shared::FromJson;
    use guardian_shared::ToJson;
    use miden_protocol::account::AccountId;
    use miden_protocol::account::delta::{AccountDelta, AccountStorageDelta, AccountVaultDelta};
    use miden_protocol::transaction::{InputNotes, RawOutputNotes, TransactionSummary};
    use miden_protocol::{Felt, Word};

    use super::MultisigClient;

    fn tx_summary_json() -> serde_json::Value {
        let account_id = AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594170").unwrap();
        let delta = AccountDelta::new(
            account_id,
            AccountStorageDelta::default(),
            AccountVaultDelta::default(),
            Felt::ZERO,
        )
        .unwrap();
        TransactionSummary::new(
            delta,
            InputNotes::new(Vec::new()).unwrap(),
            RawOutputNotes::new(Vec::new()).unwrap(),
            Word::default(),
        )
        .to_json()
    }

    #[test]
    fn proposal_id_from_delta_payload_returns_tx_summary_commitment() {
        let tx_summary = TransactionSummary::from_json(&tx_summary_json()).unwrap();
        let expected_id = crate::transaction::word_to_hex(&tx_summary.to_commitment());
        let delta_payload = serde_json::json!({
            "tx_summary": tx_summary_json(),
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": []
            }
        })
        .to_string();

        let proposal_id = MultisigClient::proposal_id_from_delta_payload(&delta_payload).unwrap();

        assert_eq!(proposal_id, expected_id);
    }

    #[test]
    fn proposal_id_from_delta_payload_rejects_missing_tx_summary() {
        let result = MultisigClient::proposal_id_from_delta_payload("{\"metadata\":{}}");

        assert!(result.is_err());
    }

    #[test]
    fn ensure_proposal_account_id_accepts_matching_account() {
        let account_id = AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594170").unwrap();

        let result = MultisigClient::ensure_proposal_account_id(
            "0x7bfb0f38b0fafa103f86a805594170",
            &account_id,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn ensure_proposal_account_id_rejects_mismatched_account() {
        let account_id = AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594170").unwrap();

        let error = MultisigClient::ensure_proposal_account_id(
            "0x8a65fc5a39e4cd106d648e3eb4ab5f",
            &account_id,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid configuration: proposal is for account 0x8a65fc5a39e4cd106d648e3eb4ab5f instead of 0x7bfb0f38b0fafa103f86a805594170"
        );
    }
}
