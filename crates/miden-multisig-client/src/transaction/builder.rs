//! Proposal builder for multisig transactions.

use miden_client::Client;
use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::asset::FungibleAsset;
use miden_objects::note::NoteId;
use private_state_manager_client::PsmClient;
use private_state_manager_shared::ToJson;

use crate::account::MultisigAccount;
use crate::error::{MultisigError, Result};
use crate::keystore::KeyManager;
use crate::payload::ProposalPayload;
use crate::proposal::{Proposal, ProposalMetadata, ProposalStatus, TransactionType};

use super::{
    build_consume_notes_transaction_request, build_p2id_transaction_request,
    build_update_psm_transaction_request, build_update_signers_transaction_request,
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
///     .build(&mut miden_client, &mut psm_client, &account, key_manager)
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

    /// Builds and submits the proposal to PSM.
    pub async fn build(
        self,
        miden_client: &mut Client<()>,
        psm_client: &mut PsmClient,
        account: &MultisigAccount,
        key_manager: &dyn KeyManager,
    ) -> Result<Proposal> {
        match self.transaction_type {
            TransactionType::AddCosigner { new_commitment } => {
                self.build_add_cosigner(
                    miden_client,
                    psm_client,
                    account,
                    new_commitment,
                    key_manager,
                )
                .await
            }
            TransactionType::RemoveCosigner { commitment } => {
                self.build_remove_cosigner(
                    miden_client,
                    psm_client,
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
                    psm_client,
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
                    psm_client,
                    account,
                    note_ids.clone(),
                    key_manager,
                )
                .await
            }
            TransactionType::SwitchPsm {
                ref new_endpoint,
                new_commitment,
            } => {
                self.build_switch_psm(
                    miden_client,
                    psm_client,
                    account,
                    new_commitment,
                    new_endpoint.clone(),
                    key_manager,
                )
                .await
            }
            TransactionType::UpdateSigners { .. } => Err(MultisigError::InvalidConfig(
                "Use AddCosigner or RemoveCosigner for signer updates".to_string(),
            )),
        }
    }

    async fn build_add_cosigner(
        &self,
        miden_client: &mut Client<()>,
        psm_client: &mut PsmClient,
        account: &MultisigAccount,
        new_commitment: Word,
        key_manager: &dyn KeyManager,
    ) -> Result<Proposal> {
        let account_id = account.id();
        let current_threshold = account.threshold()?;
        let mut current_signers = account.cosigner_commitments();

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
        )?;

        // Execute to get the TransactionSummary
        let tx_summary = execute_for_summary(miden_client, account_id, tx_request).await?;

        // Sign the transaction summary commitment
        let tx_commitment = tx_summary.to_commitment();

        // Build proposal metadata
        let signer_commitments_hex: Vec<String> = current_signers.iter().map(word_to_hex).collect();

        let metadata = ProposalMetadata {
            tx_summary_json: Some(tx_summary.to_json()),
            new_threshold: Some(new_threshold),
            signer_commitments_hex: signer_commitments_hex.clone(),
            salt_hex: Some(word_to_hex(&salt)),
            recipient_hex: None,
            faucet_id_hex: None,
            amount: None,
            note_ids_hex: Vec::new(),
            new_psm_pubkey_hex: None,
            new_psm_endpoint: None,
            required_signatures: Some(current_threshold as usize),
            collected_signatures: Some(1),
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_add_signer_metadata(
                new_threshold,
                signer_commitments_hex.clone(),
                word_to_hex(&salt),
            );

        // Push proposal to PSM
        let nonce = account.nonce() + 1;
        let response = psm_client
            .push_delta_proposal(&account_id, nonce, &payload.to_json())
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to push proposal: {}", e)))?;

        // Build the Proposal
        let proposal = Proposal {
            id: response.commitment,
            nonce,
            transaction_type: TransactionType::AddCosigner { new_commitment },
            status: ProposalStatus::Pending {
                signatures_collected: 1,
                signatures_required: current_threshold as usize,
                signers: vec![key_manager.commitment_hex()],
            },
            tx_summary,
            metadata,
        };

        Ok(proposal)
    }

    async fn build_remove_cosigner(
        &self,
        miden_client: &mut Client<()>,
        psm_client: &mut PsmClient,
        account: &MultisigAccount,
        commitment_to_remove: Word,
        key_manager: &dyn KeyManager,
    ) -> Result<Proposal> {
        let account_id = account.id();
        let current_threshold = account.threshold()?;
        let current_signers = account.cosigner_commitments();

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
        )?;

        // Execute to get the TransactionSummary
        let tx_summary = execute_for_summary(miden_client, account_id, tx_request).await?;

        // Sign the transaction summary commitment
        let tx_commitment = tx_summary.to_commitment();

        // Build proposal metadata
        let signer_commitments_hex: Vec<String> = new_signers.iter().map(word_to_hex).collect();

        let metadata = ProposalMetadata {
            tx_summary_json: Some(tx_summary.to_json()),
            new_threshold: Some(new_threshold),
            signer_commitments_hex: signer_commitments_hex.clone(),
            salt_hex: Some(word_to_hex(&salt)),
            recipient_hex: None,
            faucet_id_hex: None,
            amount: None,
            note_ids_hex: Vec::new(),
            new_psm_pubkey_hex: None,
            new_psm_endpoint: None,
            required_signatures: Some(current_threshold as usize),
            collected_signatures: Some(1),
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_remove_signer_metadata(
                new_threshold,
                signer_commitments_hex.clone(),
                word_to_hex(&salt),
            );

        // Push proposal to PSM
        let nonce = account.nonce() + 1;
        let response = psm_client
            .push_delta_proposal(&account_id, nonce, &payload.to_json())
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to push proposal: {}", e)))?;

        // Build the Proposal
        let proposal = Proposal {
            id: response.commitment,
            nonce,
            transaction_type: TransactionType::RemoveCosigner {
                commitment: commitment_to_remove,
            },
            status: ProposalStatus::Pending {
                signatures_collected: 1,
                // Use current_threshold for required signatures since on-chain code
                // verifies against the current config
                signatures_required: current_threshold as usize,
                signers: vec![key_manager.commitment_hex()],
            },
            tx_summary,
            metadata,
        };

        Ok(proposal)
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_p2id(
        &self,
        miden_client: &mut Client<()>,
        psm_client: &mut PsmClient,
        account: &MultisigAccount,
        recipient: AccountId,
        faucet_id: AccountId,
        amount: u64,
        key_manager: &dyn KeyManager,
    ) -> Result<Proposal> {
        let account_id = account.id();
        let current_threshold = account.threshold()?;

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
            new_threshold: None,
            signer_commitments_hex: Vec::new(),
            salt_hex: Some(word_to_hex(&salt)),
            recipient_hex: Some(recipient.to_string()),
            faucet_id_hex: Some(faucet_id.to_string()),
            amount: Some(amount),
            note_ids_hex: Vec::new(),
            new_psm_pubkey_hex: None,
            new_psm_endpoint: None,
            required_signatures: Some(current_threshold as usize),
            collected_signatures: Some(1),
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_payment_metadata(
                recipient.to_string(),
                faucet_id.to_string(),
                amount,
                word_to_hex(&salt),
            );

        // Push proposal to PSM
        let nonce = account.nonce() + 1;
        let response = psm_client
            .push_delta_proposal(&account_id, nonce, &payload.to_json())
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to push proposal: {}", e)))?;

        // Build the Proposal
        let proposal = Proposal {
            id: response.commitment,
            nonce,
            transaction_type: TransactionType::P2ID {
                recipient,
                faucet_id,
                amount,
            },
            status: ProposalStatus::Pending {
                signatures_collected: 1,
                signatures_required: current_threshold as usize,
                signers: vec![key_manager.commitment_hex()],
            },
            tx_summary,
            metadata,
        };

        Ok(proposal)
    }

    async fn build_consume_notes(
        &self,
        miden_client: &mut Client<()>,
        psm_client: &mut PsmClient,
        account: &MultisigAccount,
        note_ids: Vec<NoteId>,
        key_manager: &dyn KeyManager,
    ) -> Result<Proposal> {
        let account_id = account.id();
        let current_threshold = account.threshold()?;

        // Generate salt for replay protection
        let salt = generate_salt();

        // Build the consume notes transaction request (no signatures for proposal)
        let tx_request =
            build_consume_notes_transaction_request(note_ids.clone(), salt, std::iter::empty())?;

        // Execute to get the TransactionSummary
        let tx_summary = execute_for_summary(miden_client, account_id, tx_request).await?;

        // Sign the transaction summary commitment
        let tx_commitment = tx_summary.to_commitment();

        // Build proposal metadata
        let note_ids_hex: Vec<String> = note_ids.iter().map(|id| id.to_hex()).collect();

        let metadata = ProposalMetadata {
            tx_summary_json: Some(tx_summary.to_json()),
            new_threshold: None,
            signer_commitments_hex: Vec::new(),
            salt_hex: Some(word_to_hex(&salt)),
            recipient_hex: None,
            faucet_id_hex: None,
            amount: None,
            note_ids_hex: note_ids_hex.clone(),
            new_psm_pubkey_hex: None,
            new_psm_endpoint: None,
            required_signatures: Some(current_threshold as usize),
            collected_signatures: Some(1),
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_note_consumption_metadata(&note_ids_hex, word_to_hex(&salt));

        // Push proposal to PSM
        let nonce = account.nonce() + 1;
        let response = psm_client
            .push_delta_proposal(&account_id, nonce, &payload.to_json())
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to push proposal: {}", e)))?;

        // Build the Proposal
        let proposal = Proposal {
            id: response.commitment,
            nonce,
            transaction_type: TransactionType::ConsumeNotes { note_ids },
            status: ProposalStatus::Pending {
                signatures_collected: 1,
                signatures_required: current_threshold as usize,
                signers: vec![key_manager.commitment_hex()],
            },
            tx_summary,
            metadata,
        };

        Ok(proposal)
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_switch_psm(
        &self,
        miden_client: &mut Client<()>,
        psm_client: &mut PsmClient,
        account: &MultisigAccount,
        new_psm_pubkey: Word,
        new_psm_endpoint: String,
        key_manager: &dyn KeyManager,
    ) -> Result<Proposal> {
        let account_id = account.id();
        let current_threshold = account.threshold()?;

        // Generate salt for replay protection
        let salt = generate_salt();

        // Build the PSM update transaction request (no signatures for proposal)
        let tx_request =
            build_update_psm_transaction_request(new_psm_pubkey, salt, std::iter::empty())?;

        // Execute to get the TransactionSummary
        let tx_summary = execute_for_summary(miden_client, account_id, tx_request).await?;

        // Sign the transaction summary commitment
        let tx_commitment = tx_summary.to_commitment();

        // Build proposal metadata
        let metadata = ProposalMetadata {
            tx_summary_json: Some(tx_summary.to_json()),
            new_threshold: None,
            signer_commitments_hex: Vec::new(),
            salt_hex: Some(word_to_hex(&salt)),
            recipient_hex: None,
            faucet_id_hex: None,
            amount: None,
            note_ids_hex: Vec::new(),
            new_psm_pubkey_hex: Some(word_to_hex(&new_psm_pubkey)),
            new_psm_endpoint: Some(new_psm_endpoint.clone()),
            required_signatures: Some(current_threshold as usize),
            collected_signatures: Some(1),
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_psm_update_metadata(
                word_to_hex(&new_psm_pubkey),
                new_psm_endpoint.clone(),
                word_to_hex(&salt),
            );

        // Push proposal to PSM
        let nonce = account.nonce() + 1;
        let response = psm_client
            .push_delta_proposal(&account_id, nonce, &payload.to_json())
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to push proposal: {}", e)))?;

        // Build the Proposal
        let proposal = Proposal {
            id: response.commitment,
            nonce,
            transaction_type: TransactionType::SwitchPsm {
                new_endpoint: new_psm_endpoint,
                new_commitment: new_psm_pubkey,
            },
            status: ProposalStatus::Pending {
                signatures_collected: 1,
                signatures_required: current_threshold as usize,
                signers: vec![key_manager.commitment_hex()],
            },
            tx_summary,
            metadata,
        };

        Ok(proposal)
    }
}
