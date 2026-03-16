//! Offline proposal operations for MultisigClient.
//!
//! This module handles creating, signing, and executing proposals
//! without PSM coordination (offline/side-channel mode).

use std::collections::HashSet;

use private_state_manager_shared::{SignatureScheme, ToJson};
use private_state_manager_shared::hex::IntoHex;

use super::MultisigClient;
use crate::error::{MultisigError, Result};
use crate::execution::{SignatureInput, build_final_transaction_request, collect_signature_advice};
use crate::export::{EXPORT_VERSION, ExportedMetadata, ExportedProposal, ExportedSignature};
use crate::proposal::TransactionType;
use crate::psm_endpoint::verify_endpoint_commitment;

impl MultisigClient {
    /// Creates a proposal offline without pushing to PSM.
    ///
    /// Only `SwitchPsm` transactions can be executed fully offline because
    /// all other transaction types require a PSM acknowledgment signature.
    ///
    /// This returns an `ExportedProposal` that can be serialized to JSON and
    /// shared with cosigners.
    ///
    /// The proposer's signature is automatically included in the exported proposal.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use miden_multisig_client::TransactionType;
    ///
    /// // Create proposal offline
    /// let exported = client.create_proposal_offline(
    ///     TransactionType::SwitchPsm { new_endpoint, new_commitment }
    /// ).await?;
    ///
    /// // Save to file for sharing
    /// std::fs::write("proposal.json", exported.to_json()?)?;
    /// ```
    pub async fn create_proposal_offline(
        &mut self,
        transaction_type: TransactionType,
    ) -> Result<ExportedProposal> {
        self.sync_network_only().await?;

        let account = self.require_account()?.clone();
        let account_id = account.id();
        let signatures_required =
            account.effective_threshold_for_transaction(&transaction_type)? as usize;

        let salt = crate::transaction::generate_salt();
        let (new_endpoint, new_commitment) = match &transaction_type {
            TransactionType::SwitchPsm {
                new_endpoint,
                new_commitment,
            } => {
                verify_endpoint_commitment(new_endpoint, *new_commitment).await?;
                (new_endpoint.clone(), *new_commitment)
            }
            _ => {
                return Err(MultisigError::OfflineUnsupportedTransaction(
                    transaction_type.type_name().to_string(),
                ));
            }
        };

        let tx_request = crate::transaction::build_update_psm_transaction_request(
            new_commitment,
            salt,
            std::iter::empty(),
        )?;
        let metadata = ExportedMetadata {
            proposal_type: "switch_psm".to_string(),
            salt_hex: Some(crate::transaction::word_to_hex(&salt)),
            new_psm_pubkey_hex: Some(crate::transaction::word_to_hex(&new_commitment)),
            new_psm_endpoint: Some(new_endpoint),
            ..Default::default()
        };

        let tx_summary =
            crate::transaction::execute_for_summary(&mut self.miden_client, account_id, tx_request)
                .await?;

        let tx_commitment = tx_summary.to_commitment();
        let signature_hex = self.signer.sign_word(tx_commitment).into_hex();

        let id = format!(
            "0x{}",
            hex::encode(
                tx_commitment
                    .iter()
                    .flat_map(|f| f.as_int().to_le_bytes())
                    .collect::<Vec<_>>()
            )
        );

        let exported = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: account_id.to_string(),
            id,
            nonce: account.nonce() + 1,
            tx_summary: tx_summary.to_json(),
            signatures: vec![ExportedSignature {
                signer_commitment: self.signer.commitment_hex(),
                signature: signature_hex,
            }],
            signatures_required,
            metadata,
        };

        Ok(exported)
    }

    /// Signs an imported proposal locally (without PSM).
    ///
    /// The signature is added directly to the proposal. After signing,
    /// export the proposal again to share with other cosigners.
    ///
    /// Only `SwitchPsm` proposals are supported in this mode.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut proposal = client.import_proposal("/tmp/proposal.json").await?;
    /// client.sign_imported_proposal(&mut proposal).await?;
    /// let json = proposal.to_json()?;
    /// std::fs::write("/tmp/proposal_signed.json", json)?;
    /// ```
    pub async fn sign_imported_proposal(&mut self, proposal: &mut ExportedProposal) -> Result<()> {
        let bound_proposal = proposal.to_proposal()?;
        if !bound_proposal.transaction_type.supports_offline_execution() {
            return Err(MultisigError::OfflineUnsupportedTransaction(
                bound_proposal.transaction_type.type_name().to_string(),
            ));
        }
        self.verify_proposal_summary_binding(&bound_proposal)
            .await?;
        let account = self.require_account()?;
        let account_id = account.id();
        proposal.validate(Some(account_id))?;

        // Check if user is a cosigner
        let user_commitment = self.signer.commitment();
        if !account.is_cosigner(&user_commitment) {
            return Err(MultisigError::NotCosigner);
        }

        Self::ensure_proposal_account_id(&proposal.account_id, &account_id)?;

        // Check if already signed
        let user_commitment_hex = self.signer.commitment_hex();
        if proposal.signatures.iter().any(|s| {
            s.signer_commitment
                .eq_ignore_ascii_case(&user_commitment_hex)
        }) {
            return Err(MultisigError::AlreadySigned);
        }
        // Sign the transaction summary commitment
        let tx_commitment = bound_proposal.tx_summary.to_commitment();
        let signature_hex = self.signer.sign_word(tx_commitment).into_hex();

        // Add signature to proposal
        proposal.add_signature(ExportedSignature {
            signer_commitment: user_commitment_hex,
            signature: signature_hex,
        })?;

        Ok(())
    }

    /// Executes an imported proposal (with all signatures already collected).
    ///
    /// This builds and submits the transaction directly to the Miden network
    /// without contacting PSM.
    ///
    /// Only `SwitchPsm` transactions are supported in this mode.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let proposal = client.import_proposal("/tmp/proposal_final.json").await?;
    /// client.execute_imported_proposal(&proposal).await?;
    /// ```
    pub async fn execute_imported_proposal(&mut self, exported: &ExportedProposal) -> Result<()> {
        self.sync_network_only().await?;
        let account = self.require_account()?.clone();
        let account_id = account.id();
        exported.validate(Some(account_id))?;

        // Verify proposal is ready
        if !exported.is_ready() {
            return Err(MultisigError::ProposalNotReady {
                collected: exported.signatures_collected(),
                required: exported.signatures_required,
            });
        }

        // Parse the proposal
        let proposal = exported.to_proposal()?;
        if !proposal.transaction_type.supports_offline_execution() {
            return Err(MultisigError::OfflineUnsupportedTransaction(
                proposal.transaction_type.type_name().to_string(),
            ));
        }
        self.verify_proposal_summary_binding(&proposal).await?;
        let tx_summary = proposal.tx_summary.clone();
        let tx_summary_commitment = tx_summary.to_commitment();

        // Convert exported signatures to SignatureInput format
        let signature_inputs: Vec<SignatureInput> = exported
            .signatures
            .iter()
            .map(|sig| SignatureInput {
                signer_commitment: sig.signer_commitment.clone(),
                signature_hex: sig.signature.clone(),
                scheme: SignatureScheme::Falcon,
                public_key_hex: None,
            })
            .collect();

        // Build signature advice from cosigner signatures
        let required_commitments: HashSet<String> =
            account.cosigner_commitments_hex().into_iter().collect();
        let signature_advice = collect_signature_advice(
            signature_inputs,
            &required_commitments,
            tx_summary_commitment,
        )?;

        // Build the final transaction request with all signatures
        let salt = proposal.metadata.salt()?;

        let final_tx_request = build_final_transaction_request(
            &self.miden_client,
            &proposal.transaction_type,
            account.inner(),
            salt,
            signature_advice,
            None,
            None,
        )
        .await?;

        // Execute and finalize
        self.finalize_transaction(account_id, final_tx_request, &proposal.transaction_type)
            .await
    }
}
