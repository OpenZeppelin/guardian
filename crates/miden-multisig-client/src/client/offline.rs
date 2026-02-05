//! Offline proposal operations for MultisigClient.
//!
//! This module handles creating, signing, and executing proposals
//! without PSM coordination (offline/side-channel mode).

use std::collections::HashSet;

use miden_objects::asset::FungibleAsset;
use miden_objects::transaction::TransactionSummary;
use private_state_manager_shared::SignatureScheme;
use private_state_manager_shared::{FromJson, ToJson};

use super::MultisigClient;

use crate::error::{MultisigError, Result};
use crate::execution::{SignatureInput, build_final_transaction_request, collect_signature_advice};
use crate::export::{EXPORT_VERSION, ExportedMetadata, ExportedProposal, ExportedSignature};
use crate::proposal::TransactionType;
use crate::transaction::{
    build_consume_notes_transaction_request, build_p2id_transaction_request,
    build_update_psm_transaction_request, build_update_signers_transaction_request,
    execute_for_summary, generate_salt, word_to_hex,
};

impl MultisigClient {
    /// Creates a proposal offline without pushing to PSM.
    ///
    /// Use this when PSM is unavailable or you want to share proposals via
    /// side channels. The proposal is returned as an `ExportedProposal` that
    /// can be serialized to JSON and shared with cosigners.
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
        self.sync().await?;

        let account = self.require_account()?.clone();
        let account_id = account.id();
        let current_threshold = account.threshold()?;

        let salt = generate_salt();
        let salt_hex = word_to_hex(&salt);

        let (tx_request, metadata) = match &transaction_type {
            TransactionType::SwitchPsm {
                new_endpoint,
                new_commitment,
            } => {
                let tx_request = build_update_psm_transaction_request(
                    *new_commitment,
                    salt,
                    std::iter::empty(),
                )?;

                let metadata = ExportedMetadata {
                    salt_hex: Some(salt_hex.clone()),
                    new_psm_pubkey_hex: Some(word_to_hex(new_commitment)),
                    new_psm_endpoint: Some(new_endpoint.clone()),
                    ..Default::default()
                };

                (tx_request, metadata)
            }
            TransactionType::P2ID {
                recipient,
                faucet_id,
                amount,
            } => {
                let asset = FungibleAsset::new(*faucet_id, *amount).map_err(|e| {
                    MultisigError::InvalidConfig(format!("failed to create asset: {}", e))
                })?;

                let tx_request = build_p2id_transaction_request(
                    account.inner(),
                    *recipient,
                    vec![asset.into()],
                    salt,
                    std::iter::empty(),
                )?;

                let metadata = ExportedMetadata {
                    salt_hex: Some(salt_hex.clone()),
                    recipient_hex: Some(recipient.to_string()),
                    faucet_id_hex: Some(faucet_id.to_string()),
                    amount: Some(*amount),
                    ..Default::default()
                };

                (tx_request, metadata)
            }
            TransactionType::ConsumeNotes { note_ids } => {
                let tx_request = build_consume_notes_transaction_request(
                    note_ids.clone(),
                    salt,
                    std::iter::empty(),
                )?;

                let note_ids_hex: Vec<String> = note_ids.iter().map(|id| id.to_hex()).collect();
                let metadata = ExportedMetadata {
                    salt_hex: Some(salt_hex.clone()),
                    note_ids_hex,
                    ..Default::default()
                };

                (tx_request, metadata)
            }
            TransactionType::AddCosigner { new_commitment } => {
                let mut current_signers = account.cosigner_commitments();
                current_signers.push(*new_commitment);
                let new_threshold = current_threshold as u64;

                let (tx_request, _) = build_update_signers_transaction_request(
                    new_threshold,
                    &current_signers,
                    salt,
                    std::iter::empty(),
                )?;

                let signer_commitments_hex: Vec<String> =
                    current_signers.iter().map(word_to_hex).collect();

                let metadata = ExportedMetadata {
                    salt_hex: Some(salt_hex.clone()),
                    new_threshold: Some(new_threshold),
                    signer_commitments_hex,
                    ..Default::default()
                };

                (tx_request, metadata)
            }
            TransactionType::RemoveCosigner { commitment } => {
                let current_signers = account.cosigner_commitments();
                let new_signers: Vec<_> = current_signers
                    .iter()
                    .filter(|&c| c != commitment)
                    .copied()
                    .collect();

                if new_signers.len() == current_signers.len() {
                    return Err(MultisigError::InvalidConfig(
                        "commitment to remove not found in signers".to_string(),
                    ));
                }

                let new_threshold =
                    std::cmp::min(current_threshold as u64, new_signers.len() as u64);

                let (tx_request, _) = build_update_signers_transaction_request(
                    new_threshold,
                    &new_signers,
                    salt,
                    std::iter::empty(),
                )?;

                let signer_commitments_hex: Vec<String> =
                    new_signers.iter().map(word_to_hex).collect();

                let metadata = ExportedMetadata {
                    salt_hex: Some(salt_hex.clone()),
                    new_threshold: Some(new_threshold),
                    signer_commitments_hex,
                    ..Default::default()
                };

                (tx_request, metadata)
            }
            TransactionType::UpdateSigners {
                new_threshold,
                signer_commitments,
            } => {
                let (tx_request, _) = build_update_signers_transaction_request(
                    *new_threshold as u64,
                    signer_commitments,
                    salt,
                    std::iter::empty(),
                )?;

                let signer_commitments_hex: Vec<String> =
                    signer_commitments.iter().map(word_to_hex).collect();

                let metadata = ExportedMetadata {
                    salt_hex: Some(salt_hex.clone()),
                    new_threshold: Some(*new_threshold as u64),
                    signer_commitments_hex,
                    ..Default::default()
                };

                (tx_request, metadata)
            }
        };

        let tx_summary =
            execute_for_summary(&mut self.miden_client, account_id, tx_request).await?;

        let tx_commitment = tx_summary.to_commitment();
        let signature_hex = self.key_manager.sign_hex(tx_commitment);

        let id = format!(
            "0x{}",
            hex::encode(
                tx_commitment
                    .iter()
                    .flat_map(|f| f.as_int().to_le_bytes())
                    .collect::<Vec<_>>()
            )
        );

        let tx_type_str = match &transaction_type {
            TransactionType::P2ID { .. } => "P2ID",
            TransactionType::ConsumeNotes { .. } => "ConsumeNotes",
            TransactionType::AddCosigner { .. } => "AddCosigner",
            TransactionType::RemoveCosigner { .. } => "RemoveCosigner",
            TransactionType::SwitchPsm { .. } => "SwitchPsm",
            TransactionType::UpdateSigners { .. } => "UpdateSigners",
        };

        let exported = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: account_id.to_string(),
            id,
            nonce: account.nonce() + 1,
            transaction_type: tx_type_str.to_string(),
            tx_summary: tx_summary.to_json(),
            signatures: vec![ExportedSignature {
                signer_commitment: self.key_manager.commitment_hex(),
                signature: signature_hex,
                scheme: self.key_manager.scheme().to_string(),
                public_key_hex: self.key_manager.public_key_hex().unwrap_or_default(),
            }],
            signatures_required: current_threshold as usize,
            metadata,
        };

        Ok(exported)
    }

    /// Signs an imported proposal locally (without PSM).
    ///
    /// The signature is added directly to the proposal. After signing,
    /// export the proposal again to share with other cosigners.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut proposal = client.import_proposal("/tmp/proposal.json")?;
    /// client.sign_imported_proposal(&mut proposal)?;
    /// let json = proposal.to_json()?;
    /// std::fs::write("/tmp/proposal_signed.json", json)?;
    /// ```
    pub fn sign_imported_proposal(&self, proposal: &mut ExportedProposal) -> Result<()> {
        let account = self.require_account()?;

        let user_commitment = self.key_manager.commitment();
        if !account.is_cosigner(&user_commitment) {
            return Err(MultisigError::NotCosigner);
        }

        let user_commitment_hex = self.key_manager.commitment_hex();
        if proposal.signatures.iter().any(|s| {
            s.signer_commitment
                .eq_ignore_ascii_case(&user_commitment_hex)
        }) {
            return Err(MultisigError::AlreadySigned);
        }

        let tx_summary = TransactionSummary::from_json(&proposal.tx_summary).map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to parse tx_summary: {}", e))
        })?;

        let tx_commitment = tx_summary.to_commitment();
        let signature_hex = self.key_manager.sign_hex(tx_commitment);

        proposal.add_signature(ExportedSignature {
            signer_commitment: user_commitment_hex,
            signature: signature_hex,
            scheme: self.key_manager.scheme().to_string(),
            public_key_hex: self.key_manager.public_key_hex().unwrap_or_default(),
        })?;

        Ok(())
    }

    /// Executes an imported proposal (with all signatures already collected).
    ///
    /// This builds and submits the transaction directly to the Miden network,
    /// bypassing PSM entirely. Use this for fully offline workflows.
    ///
    /// **Note:** This does NOT update PSM. The proposal will remain on PSM
    /// until it expires or is explicitly deleted.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let proposal = client.import_proposal("/tmp/proposal_final.json")?;
    /// client.execute_imported_proposal(&proposal).await?;
    /// ```
    pub async fn execute_imported_proposal(&mut self, exported: &ExportedProposal) -> Result<()> {
        self.sync().await?;

        let account = self.require_account()?.clone();
        let account_id = account.id();

        if !exported.is_ready() {
            return Err(MultisigError::ProposalNotReady {
                collected: exported.signatures_collected(),
                required: exported.signatures_required,
            });
        }

        let proposal = exported.to_proposal()?;
        let tx_summary = TransactionSummary::from_json(&exported.tx_summary).map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to parse tx_summary: {}", e))
        })?;
        let tx_summary_commitment = tx_summary.to_commitment();

        let signature_inputs: Vec<SignatureInput> = exported
            .signatures
            .iter()
            .map(|sig| {
                let scheme = match sig.scheme.as_str() {
                    "ecdsa" => SignatureScheme::Ecdsa,
                    _ => SignatureScheme::Falcon,
                };
                SignatureInput {
                    signer_commitment: sig.signer_commitment.clone(),
                    signature_hex: sig.signature.clone(),
                    scheme,
                    public_key_hex: if sig.public_key_hex.is_empty() {
                        None
                    } else {
                        Some(sig.public_key_hex.clone())
                    },
                }
            })
            .collect();

        let required_commitments: HashSet<String> =
            account.cosigner_commitments_hex().into_iter().collect();
        let mut signature_advice = collect_signature_advice(
            signature_inputs,
            &required_commitments,
            tx_summary_commitment,
        )?;

        let is_switch_psm = matches!(
            &proposal.transaction_type,
            TransactionType::SwitchPsm { .. }
        );

        if !is_switch_psm {
            let psm_advice = self
                .get_psm_ack_signature(&account, proposal.nonce, &tx_summary, tx_summary_commitment)
                .await?;
            signature_advice.push(psm_advice);
        }

        let salt = proposal.metadata.salt()?;

        let signer_commitments = if matches!(
            &proposal.transaction_type,
            TransactionType::AddCosigner { .. }
                | TransactionType::RemoveCosigner { .. }
                | TransactionType::UpdateSigners { .. }
        ) {
            Some(proposal.metadata.signer_commitments()?)
        } else {
            proposal.metadata.signer_commitments().ok()
        };

        let final_tx_request = build_final_transaction_request(
            &proposal.transaction_type,
            account.inner(),
            salt,
            signature_advice,
            proposal.metadata.new_threshold,
            signer_commitments.as_deref(),
        )?;

        self.finalize_transaction(account_id, final_tx_request, &proposal.transaction_type)
            .await
    }
}
