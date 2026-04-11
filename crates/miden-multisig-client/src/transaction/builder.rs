//! Proposal builder for multisig transactions.

use guardian_client::GuardianClient;
use guardian_shared::ToJson;
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::asset::FungibleAsset;
use miden_protocol::note::NoteId;

use crate::MidenSdkClient;
use crate::account::MultisigAccount;
use crate::error::{MultisigError, Result};
use crate::guardian_endpoint::verify_endpoint_commitment;
use crate::keystore::{KeyManager, ensure_hex_prefix};
use crate::payload::ProposalPayload;
use crate::procedures::ProcedureName;
use crate::proposal::{Proposal, ProposalMetadata, TransactionType};
use crate::utils::hex_body_eq;

use super::{
    build_consume_notes_transaction_request, build_p2id_transaction_request,
    build_update_guardian_transaction_request,
    build_update_procedure_threshold_transaction_request, build_update_signers_transaction_request,
    execute_for_summary, generate_salt, word_to_hex,
};

/// Builder for creating multisig transaction proposals.
///
/// # Example
///
/// ```ignore
/// use miden_multisig_client::TransactionType;
///
/// let proposal = ProposalBuilder::new(TransactionType::AddCosigner { new_commitment })
///     .build(&mut miden_client, &mut guardian_client, &account, key_manager)
///     .await?;
/// ```
pub struct ProposalBuilder {
    transaction_type: TransactionType,
}

impl ProposalBuilder {
    /// Creates a new proposal builder for the given transaction type.
    pub fn new(transaction_type: TransactionType) -> Self {
        Self { transaction_type }
    }

    /// Builds and submits the proposal to GUARDIAN.
    pub async fn build(
        self,
        miden_client: &mut MidenSdkClient,
        guardian_client: &mut GuardianClient,
        account: &MultisigAccount,
        key_manager: &dyn KeyManager,
    ) -> Result<Proposal> {
        match self.transaction_type {
            TransactionType::AddCosigner { new_commitment } => {
                self.build_add_cosigner(
                    miden_client,
                    guardian_client,
                    account,
                    new_commitment,
                    key_manager,
                )
                .await
            }
            TransactionType::RemoveCosigner { commitment } => {
                self.build_remove_cosigner(
                    miden_client,
                    guardian_client,
                    account,
                    commitment,
                    key_manager,
                )
                .await
            }
            TransactionType::P2ID {
                recipient,
                faucet_id,
                amount,
            } => {
                self.build_p2id(
                    miden_client,
                    guardian_client,
                    account,
                    recipient,
                    faucet_id,
                    amount,
                    key_manager,
                )
                .await
            }
            TransactionType::ConsumeNotes { ref note_ids } => {
                self.build_consume_notes(
                    miden_client,
                    guardian_client,
                    account,
                    note_ids.clone(),
                    key_manager,
                )
                .await
            }
            TransactionType::SwitchGuardian {
                ref new_endpoint,
                new_commitment,
            } => {
                self.build_switch_guardian(
                    miden_client,
                    guardian_client,
                    account,
                    new_commitment,
                    new_endpoint.clone(),
                    key_manager,
                )
                .await
            }
            TransactionType::UpdateProcedureThreshold {
                procedure,
                new_threshold,
            } => {
                self.build_update_procedure_threshold(
                    miden_client,
                    guardian_client,
                    account,
                    procedure,
                    new_threshold,
                    key_manager,
                )
                .await
            }
            TransactionType::UpdateSigners { .. } => Err(MultisigError::InvalidConfig(
                "Use AddCosigner or RemoveCosigner for signer updates".to_string(),
            )),
        }
    }

    fn ensure_response_commitment(proposal: &Proposal, response_commitment: &str) -> Result<()> {
        let response_commitment = ensure_hex_prefix(response_commitment);
        if hex_body_eq(&proposal.id, &response_commitment) {
            return Ok(());
        }

        Err(MultisigError::GuardianServer(format!(
            "GUARDIAN returned proposal commitment {} but transaction summary commitment is {}",
            response_commitment, proposal.id
        )))
    }

    async fn build_add_cosigner(
        &self,
        miden_client: &mut MidenSdkClient,
        guardian_client: &mut GuardianClient,
        account: &MultisigAccount,
        new_commitment: Word,
        key_manager: &dyn KeyManager,
    ) -> Result<Proposal> {
        let account_id = account.id();
        let current_threshold = account.threshold()?;
        let mut current_signers = account.cosigner_commitments();
        let required_signatures =
            account.effective_threshold_for_procedure(ProcedureName::UpdateSigners)? as usize;

        // Add the new signer
        current_signers.push(new_commitment);

        // Keep same threshold
        let new_threshold = current_threshold as u64;

        // Generate salt for replay protection
        let salt = generate_salt();

        // Build the transaction request (without signatures - we just want the summary)
        let (tx_request, _config_hash) = build_update_signers_transaction_request(
            new_threshold,
            &current_signers,
            salt,
            std::iter::empty(),
            key_manager.scheme(),
        )?;

        // Execute to get the TransactionSummary
        let tx_summary = execute_for_summary(miden_client, account_id, tx_request).await?;

        // Sign the transaction summary commitment
        let tx_commitment = tx_summary.to_commitment();

        // Build proposal metadata
        let signer_commitments_hex: Vec<String> = current_signers.iter().map(word_to_hex).collect();

        let metadata = ProposalMetadata {
            tx_summary_json: Some(tx_summary.to_json()),
            proposal_type: None,
            new_threshold: Some(new_threshold),
            signer_commitments_hex: signer_commitments_hex.clone(),
            salt_hex: Some(word_to_hex(&salt)),
            recipient_hex: None,
            faucet_id_hex: None,
            amount: None,
            note_ids_hex: Vec::new(),
            new_guardian_pubkey_hex: None,
            new_guardian_endpoint: None,
            target_procedure: None,
            required_signatures: Some(required_signatures),
            signers: vec![key_manager.commitment_hex()],
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_add_signer_metadata(
                new_threshold,
                signer_commitments_hex.clone(),
                word_to_hex(&salt),
            )
            .with_required_signatures(required_signatures);

        // Push proposal to GUARDIAN
        let nonce = account.nonce() + 1;
        let response = guardian_client
            .push_delta_proposal(&account_id, nonce, &payload.to_json())
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!("failed to push proposal: {}", e))
            })?;

        // Build the Proposal
        let proposal = Proposal::new(
            tx_summary,
            nonce,
            TransactionType::AddCosigner { new_commitment },
            metadata,
        );
        Self::ensure_response_commitment(&proposal, &response.commitment)?;

        Ok(proposal)
    }

    async fn build_remove_cosigner(
        &self,
        miden_client: &mut MidenSdkClient,
        guardian_client: &mut GuardianClient,
        account: &MultisigAccount,
        commitment_to_remove: Word,
        key_manager: &dyn KeyManager,
    ) -> Result<Proposal> {
        let account_id = account.id();
        let current_threshold = account.threshold()?;
        let current_signers = account.cosigner_commitments();
        let required_signatures =
            account.effective_threshold_for_procedure(ProcedureName::UpdateSigners)? as usize;

        // Remove the signer
        let new_signers: Vec<Word> = current_signers
            .iter()
            .filter(|&c| c != &commitment_to_remove)
            .copied()
            .collect();

        if new_signers.len() == current_signers.len() {
            return Err(MultisigError::InvalidConfig(
                "commitment to remove not found in signers".to_string(),
            ));
        }

        // Adjust threshold if needed (can't be more than signers)
        let new_threshold = std::cmp::min(current_threshold as u64, new_signers.len() as u64);

        if new_signers.is_empty() {
            return Err(MultisigError::InvalidConfig(
                "cannot remove last signer".to_string(),
            ));
        }

        // Generate salt for replay protection
        let salt = generate_salt();

        // Build the transaction request
        let (tx_request, _config_hash) = build_update_signers_transaction_request(
            new_threshold,
            &new_signers,
            salt,
            std::iter::empty(),
            key_manager.scheme(),
        )?;

        // Execute to get the TransactionSummary
        let tx_summary = execute_for_summary(miden_client, account_id, tx_request).await?;

        // Sign the transaction summary commitment
        let tx_commitment = tx_summary.to_commitment();

        // Build proposal metadata
        let signer_commitments_hex: Vec<String> = new_signers.iter().map(word_to_hex).collect();

        let metadata = ProposalMetadata {
            tx_summary_json: Some(tx_summary.to_json()),
            proposal_type: None,
            new_threshold: Some(new_threshold),
            signer_commitments_hex: signer_commitments_hex.clone(),
            salt_hex: Some(word_to_hex(&salt)),
            recipient_hex: None,
            faucet_id_hex: None,
            amount: None,
            note_ids_hex: Vec::new(),
            new_guardian_pubkey_hex: None,
            new_guardian_endpoint: None,
            target_procedure: None,
            required_signatures: Some(required_signatures),
            signers: vec![key_manager.commitment_hex()],
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_remove_signer_metadata(
                new_threshold,
                signer_commitments_hex.clone(),
                word_to_hex(&salt),
            )
            .with_required_signatures(required_signatures);

        // Push proposal to GUARDIAN
        let nonce = account.nonce() + 1;
        let response = guardian_client
            .push_delta_proposal(&account_id, nonce, &payload.to_json())
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!("failed to push proposal: {}", e))
            })?;

        // Build the Proposal
        let proposal = Proposal::new(
            tx_summary,
            nonce,
            TransactionType::RemoveCosigner {
                commitment: commitment_to_remove,
            },
            metadata,
        );
        Self::ensure_response_commitment(&proposal, &response.commitment)?;

        Ok(proposal)
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_p2id(
        &self,
        miden_client: &mut MidenSdkClient,
        guardian_client: &mut GuardianClient,
        account: &MultisigAccount,
        recipient: AccountId,
        faucet_id: AccountId,
        amount: u64,
        key_manager: &dyn KeyManager,
    ) -> Result<Proposal> {
        let account_id = account.id();
        let required_signatures =
            account.effective_threshold_for_procedure(ProcedureName::SendAsset)? as usize;

        // Create the fungible asset
        let asset = FungibleAsset::new(faucet_id, amount)
            .map_err(|e| MultisigError::InvalidConfig(format!("failed to create asset: {}", e)))?;

        // Generate salt for replay protection
        let salt = generate_salt();

        // Build the P2ID transaction request (no signature advice needed for proposal)
        let tx_request = build_p2id_transaction_request(
            account.inner(),
            recipient,
            vec![asset.into()],
            salt,
            std::iter::empty(),
        )?;

        // Execute to get the TransactionSummary
        let tx_summary = execute_for_summary(miden_client, account_id, tx_request).await?;

        // Sign the transaction summary commitment
        let tx_commitment = tx_summary.to_commitment();

        // Build proposal metadata
        let metadata = ProposalMetadata {
            tx_summary_json: Some(tx_summary.to_json()),
            proposal_type: None,
            new_threshold: None,
            signer_commitments_hex: Vec::new(),
            salt_hex: Some(word_to_hex(&salt)),
            recipient_hex: Some(recipient.to_string()),
            faucet_id_hex: Some(faucet_id.to_string()),
            amount: Some(amount),
            note_ids_hex: Vec::new(),
            new_guardian_pubkey_hex: None,
            new_guardian_endpoint: None,
            target_procedure: None,
            required_signatures: Some(required_signatures),
            signers: vec![key_manager.commitment_hex()],
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_payment_metadata(
                recipient.to_string(),
                faucet_id.to_string(),
                amount,
                word_to_hex(&salt),
            )
            .with_required_signatures(required_signatures);

        // Push proposal to GUARDIAN
        let nonce = account.nonce() + 1;
        let response = guardian_client
            .push_delta_proposal(&account_id, nonce, &payload.to_json())
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!("failed to push proposal: {}", e))
            })?;

        // Build the Proposal
        let proposal = Proposal::new(
            tx_summary,
            nonce,
            TransactionType::P2ID {
                recipient,
                faucet_id,
                amount,
            },
            metadata,
        );
        Self::ensure_response_commitment(&proposal, &response.commitment)?;

        Ok(proposal)
    }

    async fn build_consume_notes(
        &self,
        miden_client: &mut MidenSdkClient,
        guardian_client: &mut GuardianClient,
        account: &MultisigAccount,
        note_ids: Vec<NoteId>,
        key_manager: &dyn KeyManager,
    ) -> Result<Proposal> {
        let account_id = account.id();
        let required_signatures =
            account.effective_threshold_for_procedure(ProcedureName::ReceiveAsset)? as usize;

        // Generate salt for replay protection
        let salt = generate_salt();

        // Build the consume notes transaction request (no signatures for proposal)
        let tx_request = build_consume_notes_transaction_request(
            miden_client,
            note_ids.clone(),
            salt,
            std::iter::empty(),
        )
        .await?;

        // Execute to get the TransactionSummary
        let tx_summary = execute_for_summary(miden_client, account_id, tx_request).await?;

        // Sign the transaction summary commitment
        let tx_commitment = tx_summary.to_commitment();

        // Build proposal metadata
        let note_ids_hex: Vec<String> = note_ids.iter().map(|id| id.to_hex()).collect();

        let metadata = ProposalMetadata {
            tx_summary_json: Some(tx_summary.to_json()),
            proposal_type: None,
            new_threshold: None,
            signer_commitments_hex: Vec::new(),
            salt_hex: Some(word_to_hex(&salt)),
            recipient_hex: None,
            faucet_id_hex: None,
            amount: None,
            note_ids_hex: note_ids_hex.clone(),
            new_guardian_pubkey_hex: None,
            new_guardian_endpoint: None,
            target_procedure: None,
            required_signatures: Some(required_signatures),
            signers: vec![key_manager.commitment_hex()],
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_note_consumption_metadata(&note_ids_hex, word_to_hex(&salt))
            .with_required_signatures(required_signatures);

        // Push proposal to GUARDIAN
        let nonce = account.nonce() + 1;
        let response = guardian_client
            .push_delta_proposal(&account_id, nonce, &payload.to_json())
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!("failed to push proposal: {}", e))
            })?;

        // Build the Proposal
        let proposal = Proposal::new(
            tx_summary,
            nonce,
            TransactionType::ConsumeNotes { note_ids },
            metadata,
        );
        Self::ensure_response_commitment(&proposal, &response.commitment)?;

        Ok(proposal)
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_switch_guardian(
        &self,
        miden_client: &mut MidenSdkClient,
        guardian_client: &mut GuardianClient,
        account: &MultisigAccount,
        new_guardian_pubkey: Word,
        new_guardian_endpoint: String,
        key_manager: &dyn KeyManager,
    ) -> Result<Proposal> {
        let account_id = account.id();
        let required_signatures =
            account.effective_threshold_for_procedure(ProcedureName::UpdateGuardian)? as usize;

        verify_endpoint_commitment(&new_guardian_endpoint, new_guardian_pubkey).await?;

        // Generate salt for replay protection
        let salt = generate_salt();

        // Build the GUARDIAN update transaction request (no signatures for proposal)
        let tx_request = build_update_guardian_transaction_request(
            new_guardian_pubkey,
            salt,
            std::iter::empty(),
        )?;

        // Execute to get the TransactionSummary
        let tx_summary = execute_for_summary(miden_client, account_id, tx_request).await?;

        // Sign the transaction summary commitment
        let tx_commitment = tx_summary.to_commitment();

        // Build proposal metadata
        let metadata = ProposalMetadata {
            tx_summary_json: Some(tx_summary.to_json()),
            proposal_type: None,
            new_threshold: None,
            signer_commitments_hex: Vec::new(),
            salt_hex: Some(word_to_hex(&salt)),
            recipient_hex: None,
            faucet_id_hex: None,
            amount: None,
            note_ids_hex: Vec::new(),
            new_guardian_pubkey_hex: Some(word_to_hex(&new_guardian_pubkey)),
            new_guardian_endpoint: Some(new_guardian_endpoint.clone()),
            target_procedure: None,
            required_signatures: Some(required_signatures),
            signers: vec![key_manager.commitment_hex()],
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_guardian_update_metadata(
                word_to_hex(&new_guardian_pubkey),
                new_guardian_endpoint.clone(),
                word_to_hex(&salt),
            )
            .with_required_signatures(required_signatures);

        // Push proposal to GUARDIAN
        let nonce = account.nonce() + 1;
        let response = guardian_client
            .push_delta_proposal(&account_id, nonce, &payload.to_json())
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!("failed to push proposal: {}", e))
            })?;

        // Build the Proposal
        let proposal = Proposal::new(
            tx_summary,
            nonce,
            TransactionType::SwitchGuardian {
                new_endpoint: new_guardian_endpoint,
                new_commitment: new_guardian_pubkey,
            },
            metadata,
        );
        Self::ensure_response_commitment(&proposal, &response.commitment)?;

        Ok(proposal)
    }

    async fn build_update_procedure_threshold(
        &self,
        miden_client: &mut MidenSdkClient,
        guardian_client: &mut GuardianClient,
        account: &MultisigAccount,
        procedure: ProcedureName,
        new_threshold: u32,
        key_manager: &dyn KeyManager,
    ) -> Result<Proposal> {
        let account_id = account.id();
        let required_signatures = account
            .effective_threshold_for_procedure(ProcedureName::UpdateProcedureThreshold)?
            as usize;

        let salt = generate_salt();
        let (tx_request, _) = build_update_procedure_threshold_transaction_request(
            procedure,
            new_threshold,
            salt,
            std::iter::empty(),
            key_manager.scheme(),
        )?;
        let tx_summary = execute_for_summary(miden_client, account_id, tx_request).await?;
        let tx_commitment = tx_summary.to_commitment();

        let metadata = ProposalMetadata {
            tx_summary_json: Some(tx_summary.to_json()),
            proposal_type: None,
            new_threshold: Some(new_threshold as u64),
            signer_commitments_hex: Vec::new(),
            salt_hex: Some(word_to_hex(&salt)),
            recipient_hex: None,
            faucet_id_hex: None,
            amount: None,
            note_ids_hex: Vec::new(),
            new_guardian_pubkey_hex: None,
            new_guardian_endpoint: None,
            target_procedure: Some(procedure.to_string()),
            required_signatures: Some(required_signatures),
            signers: vec![key_manager.commitment_hex()],
        };

        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_procedure_threshold_metadata(procedure, new_threshold as u64, word_to_hex(&salt))
            .with_required_signatures(required_signatures);

        let nonce = account.nonce() + 1;
        let response = guardian_client
            .push_delta_proposal(&account_id, nonce, &payload.to_json())
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!("failed to push proposal: {}", e))
            })?;

        let proposal = Proposal::new(
            tx_summary,
            nonce,
            TransactionType::UpdateProcedureThreshold {
                procedure,
                new_threshold,
            },
            metadata,
        );
        Self::ensure_response_commitment(&proposal, &response.commitment)?;

        Ok(proposal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_protocol::account::delta::{AccountDelta, AccountStorageDelta, AccountVaultDelta};
    use miden_protocol::transaction::{InputNotes, RawOutputNotes, TransactionSummary};
    use miden_protocol::{Felt, ZERO};

    fn test_proposal() -> Proposal {
        let account_id =
            AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594170").expect("valid account id");
        let account_delta = AccountDelta::new(
            account_id,
            AccountStorageDelta::default(),
            AccountVaultDelta::default(),
            Felt::ZERO,
        )
        .expect("valid delta");
        let tx_summary = TransactionSummary::new(
            account_delta,
            InputNotes::new(Vec::new()).expect("empty input notes"),
            RawOutputNotes::new(Vec::new()).expect("empty output notes"),
            Word::from([Felt::new(9), ZERO, ZERO, ZERO]),
        );

        Proposal::new(
            tx_summary,
            1,
            TransactionType::ConsumeNotes {
                note_ids: vec![miden_protocol::note::NoteId::from_raw(Word::from([
                    Felt::new(1),
                    ZERO,
                    ZERO,
                    ZERO,
                ]))],
            },
            ProposalMetadata {
                note_ids_hex: vec![
                    "0x0100000000000000000000000000000000000000000000000000000000000000"
                        .to_string(),
                ],
                required_signatures: Some(1),
                ..Default::default()
            },
        )
    }

    #[test]
    fn ensure_response_commitment_rejects_mismatch() {
        let proposal = test_proposal();
        let result = ProposalBuilder::ensure_response_commitment(
            &proposal,
            "0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
        );

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("transaction summary commitment")
        );
    }
}
