//! Side-channel proposal operations for MultisigClient.
//!
//! These move a proposal between cosigners as a document rather than through
//! GUARDIAN's pending set. That is off-channel signature collection, not
//! air-gapped operation: only `SwitchGuardian` executes without contacting
//! GUARDIAN at all, and only `SwitchGuardian` and `Custom` skip the binding
//! check that reproduces the transaction. Every other type needs a synced store
//! to verify, and an acknowledgement from GUARDIAN to execute.

use std::collections::HashSet;

use super::MultisigClient;
use crate::error::{MultisigError, Result};
use crate::execution::{SignatureInput, build_final_transaction_request, collect_signature_advice};
use crate::export::{EXPORT_VERSION, ExportedMetadata, ExportedProposal, ExportedSignature};
use crate::guardian_endpoint::verify_endpoint_commitment;
use crate::keystore::proposal_public_key_hex;
use crate::proposal::TransactionType;
use guardian_shared::ToJson;

impl MultisigClient {
    /// Creates a proposal offline without pushing to GUARDIAN.
    ///
    /// Only `SwitchGuardian` transactions can be executed fully offline because
    /// all other transaction types require a GUARDIAN acknowledgment signature.
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
    ///     TransactionType::SwitchGuardian { new_endpoint, new_commitment }
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
            TransactionType::SwitchGuardian {
                new_endpoint,
                new_commitment,
            } => {
                verify_endpoint_commitment(
                    new_endpoint,
                    *new_commitment,
                    self.key_manager.scheme(),
                )
                .await?;
                (new_endpoint.clone(), *new_commitment)
            }
            _ => {
                return Err(MultisigError::OfflineUnsupportedTransaction(
                    transaction_type.type_name().to_string(),
                ));
            }
        };

        let tx_request = crate::transaction::build_update_guardian_transaction_request(
            new_commitment,
            self.key_manager.scheme(),
            salt,
            std::iter::empty(),
        )?;

        let (tx_summary, chain_anchor) =
            crate::transaction::execute_for_summary(&mut self.miden_client, account_id, tx_request)
                .await?;

        let metadata = ExportedMetadata {
            proposal_type: "switch_guardian".to_string(),
            salt_hex: Some(crate::transaction::word_to_hex(&salt)),
            new_guardian_pubkey_hex: Some(crate::transaction::word_to_hex(&new_commitment)),
            new_guardian_endpoint: Some(new_endpoint),
            chain_anchor: Some(crate::transaction::chain_anchor_to_base64(&chain_anchor)),
            ..Default::default()
        };

        let tx_commitment = tx_summary.to_commitment();
        let signature_hex = self.key_manager.sign_word_hex(tx_commitment);

        let id = crate::transaction::word_to_hex(&tx_commitment);

        let exported = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: account_id.to_string(),
            id,
            nonce: account.nonce() + 1,
            tx_summary: tx_summary.to_json(),
            signatures: vec![ExportedSignature {
                signer_commitment: self.key_manager.commitment_hex(),
                signature: signature_hex,
                scheme: self.key_manager.scheme(),
                public_key_hex: proposal_public_key_hex(self.key_manager.as_ref()),
            }],
            signatures_required,
            metadata,
        };

        Ok(exported)
    }

    /// Signs an imported proposal without going through GUARDIAN.
    ///
    /// The signature is added directly to the proposal. After signing,
    /// export the proposal again to share with other cosigners.
    ///
    /// Any proposal type may be signed this way. Signing is a local act over a
    /// commitment, so it does not depend on whether execution will later need a
    /// GUARDIAN acknowledgement, which is a separate question
    /// `execute_imported_proposal` asks for itself.
    ///
    /// This is off-channel signing, not air-gapped signing. Except for
    /// `SwitchGuardian` and `Custom`, the binding check below reproduces the
    /// transaction to confirm the summary is what the metadata claims, so the
    /// signer still needs a synced store and, for consume-notes, the node.
    /// Verification is what decides whether a proposal can be signed here; a
    /// type whose summary cannot be reproduced fails there rather than being
    /// turned away in advance.
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
        let mut bound_proposal = proposal.to_proposal()?;
        self.verify_proposal_summary_binding(&mut bound_proposal)
            .await?;
        let account = self.require_account()?;
        let account_id = account.id();
        proposal.validate(Some(account_id))?;

        // Check if user is a cosigner
        let user_commitment = self.key_manager.commitment();
        if !account.is_cosigner(&user_commitment) {
            return Err(MultisigError::NotCosigner);
        }

        Self::ensure_proposal_account_id(&proposal.account_id, &account_id)?;

        // Check if already signed
        let user_commitment_hex = self.key_manager.commitment_hex();
        if proposal.signatures.iter().any(|s| {
            s.signer_commitment
                .eq_ignore_ascii_case(&user_commitment_hex)
        }) {
            return Err(MultisigError::AlreadySigned);
        }
        // Sign the transaction summary commitment
        let tx_commitment = bound_proposal.tx_summary.to_commitment();
        let signature_hex = self.key_manager.sign_word_hex(tx_commitment);

        // Add signature to proposal
        proposal.add_signature(ExportedSignature {
            signer_commitment: user_commitment_hex,
            signature: signature_hex,
            scheme: self.key_manager.scheme(),
            public_key_hex: proposal_public_key_hex(self.key_manager.as_ref()),
        })?;

        Ok(())
    }

    /// Executes an imported proposal whose signatures were collected off-channel.
    ///
    /// Cosigner signatures come from the document; the acknowledgement comes
    /// from GUARDIAN, as on the online path. **This contacts GUARDIAN for every
    /// proposal type except `SwitchGuardian`**, which is the only type that
    /// executes without an acknowledgement. Do not treat this as an air-gapped
    /// path: a transfer executed here will reach GUARDIAN over the network.
    ///
    /// Every modeled type may be executed except `Custom`, which is refused:
    /// this path builds the transaction from the proposal's type, and an
    /// arbitrary producer transaction is precisely what the SDK cannot rebuild.
    /// A custom proposal is executed with
    /// [`MultisigClient::prepare_custom_execution`] and the integration's own
    /// request. Verification reproduces the transaction for every type but
    /// `SwitchGuardian` and `Custom`, so the caller needs a synced store and,
    /// for consume-notes, the node.
    ///
    /// For `SwitchGuardian` only, deliberately skips the pre-switch
    /// proposal-note import (issue #417): it would contact the very GUARDIAN
    /// that flow exists to avoid. When the old GUARDIAN is in fact still
    /// reachable, call
    /// [`MultisigClient::preserve_pre_switch_proposal_notes`] before
    /// executing.
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
        let mut proposal = exported.to_proposal()?;

        // Refused here, before anything reaches GUARDIAN. `Custom` requires an
        // acknowledgement like any other non-switch type, and obtaining one
        // pushes the delta, so running on would leave the account holding a
        // candidate at this nonce and *then* fail locally on a transaction the
        // SDK was never able to build. The caller would be left to discover the
        // candidate and abandon it, having been told only that the type is
        // unsupported.
        if !proposal
            .transaction_type
            .executable_from_exported_document()
        {
            return Err(MultisigError::UnsupportedTransactionType(
                "a custom proposal cannot be executed from an exported document; collect the \
                 signatures, then call prepare_custom_execution and submit the producer's own \
                 transaction"
                    .to_string(),
            ));
        }

        self.verify_proposal_summary_binding(&mut proposal).await?;
        let tx_summary = proposal.tx_summary.clone();
        let tx_summary_commitment = tx_summary.to_commitment();

        // Convert exported signatures to SignatureInput format
        let signature_inputs: Vec<SignatureInput> = exported
            .signatures
            .iter()
            .map(|sig| SignatureInput {
                signer_commitment: sig.signer_commitment.clone(),
                signature_hex: sig.signature.clone(),
                scheme: sig.scheme,
                public_key_hex: sig.public_key_hex.clone(),
            })
            .collect();

        // Build signature advice from cosigner signatures
        let required_commitments: HashSet<String> =
            account.cosigner_commitments_hex().into_iter().collect();
        let mut signature_advice = collect_signature_advice(
            signature_inputs,
            &required_commitments,
            tx_summary_commitment,
        )?;

        // Cosigner signatures come from the document; the acknowledgement comes
        // from GUARDIAN, exactly as on the online path. Without this the
        // document could only ever execute a guardian switch, the one type that
        // needs no acknowledgement, which is what tied off-channel signature
        // collection to that single type.
        if proposal.transaction_type.requires_guardian_ack() {
            let guardian_advice = self
                .get_guardian_ack_signature(
                    &account,
                    proposal.nonce,
                    &tx_summary,
                    tx_summary_commitment,
                )
                .await?;
            signature_advice.push(guardian_advice);
        }

        // Build the final transaction request with all signatures
        let salt = proposal.metadata.salt()?;

        // Execute and finalize at the proposal's anchored reference block; the
        // anchor was checked against the signed summary's block commitment in
        // `verify_proposal_summary_binding` above. It also carries the fee
        // faucet used to derive native fee conversion info during execution.
        let chain_anchor = proposal.metadata.chain_anchor()?;

        let final_tx_request = build_final_transaction_request(
            &self.miden_client,
            &proposal.transaction_type,
            account.inner(),
            salt,
            signature_advice,
            None,
            None,
            self.key_manager.scheme(),
        )
        .await?;

        // Switch-only, and keyed on the type rather than on "ack-less" so a
        // future ack-less type does not inherit it. Make the deliberate import
        // skip observable rather than a silent loss when the old GUARDIAN was in
        // fact still reachable.
        if matches!(
            proposal.transaction_type,
            crate::proposal::TransactionType::SwitchGuardian { .. }
        ) {
            tracing::warn!(
                "offline switch execution skips the pre-switch proposal-note import; if the \
                 old GUARDIAN is still reachable, call preserve_pre_switch_proposal_notes \
                 before executing to keep notes embedded in its pending proposals"
            );
        }

        self.finalize_transaction(
            account_id,
            final_tx_request,
            &proposal.transaction_type,
            chain_anchor,
        )
        .await
    }
}
