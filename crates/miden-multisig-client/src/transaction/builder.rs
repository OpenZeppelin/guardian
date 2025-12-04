//! Proposal builder for multisig transactions.

use miden_client::Client;
use miden_objects::Word;
use private_state_manager_client::PsmClient;
use private_state_manager_shared::ToJson;

use crate::account::MultisigAccount;
use crate::error::{MultisigError, Result};
use crate::keystore::KeyManager;
use crate::payload::ProposalPayload;
use crate::proposal::{Proposal, ProposalMetadata, ProposalStatus, TransactionType};

use super::{
    build_update_signers_transaction_request, execute_for_summary, generate_salt, word_to_hex,
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
            TransactionType::P2ID { .. } => Err(MultisigError::InvalidConfig(
                "P2ID transfers not yet implemented".to_string(),
            )),
            TransactionType::SwitchPsm { .. } => Err(MultisigError::InvalidConfig(
                "PSM switching not yet implemented".to_string(),
            )),
            TransactionType::UpdateSigners { .. } => Err(MultisigError::InvalidConfig(
                "Use AddCosigner or RemoveCosigner for signer updates".to_string(),
            )),
            TransactionType::Unknown => Err(MultisigError::InvalidConfig(
                "Unknown transaction type".to_string(),
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
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_signer_metadata(new_threshold, signer_commitments_hex.clone(), word_to_hex(&salt));

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
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_signer_metadata(new_threshold, signer_commitments_hex.clone(), word_to_hex(&salt));

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
                signatures_required: current_threshold as usize,
                signers: vec![key_manager.commitment_hex()],
            },
            tx_summary,
            metadata,
        };

        Ok(proposal)
    }
}
