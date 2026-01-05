//! Proposal workflow operations for MultisigClient.
//!
//! This module handles listing, signing, executing, and creating proposals
//! via PSM (online mode).

use std::collections::HashSet;

use private_state_manager_client::delta_status::Status;
use private_state_manager_shared::ProposalSignature;

use super::{MultisigClient, ProposalResult};
use crate::error::{MultisigError, Result};
use crate::execution::{SignatureInput, build_final_transaction_request, collect_signature_advice};
use crate::proposal::{Proposal, TransactionType};
use crate::transaction::ProposalBuilder;

impl MultisigClient {
    /// Lists pending proposals for the current account.
    ///
    /// # Errors
    ///
    /// Returns an error if any proposal from PSM cannot be parsed. This ensures
    /// malformed PSM payloads are surfaced rather than silently dropped.
    pub async fn list_proposals(&mut self) -> Result<Vec<Proposal>> {
        let account = self.require_account()?;
        let account_id = account.id();

        let mut psm_client = self.create_authenticated_psm_client().await?;

        let current_threshold = account.threshold()?;
        let current_signers = account.cosigner_commitments();

        let response = psm_client
            .get_delta_proposals(&account_id)
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to get proposals: {}", e)))?;

        // Parse all proposals, propagating any parse errors rather than silently dropping them.
        // This ensures malformed PSM payloads are surfaced for debugging.
        let proposals: Result<Vec<Proposal>> = response
            .proposals
            .iter()
            .map(|delta| Proposal::from(delta, current_threshold, &current_signers))
            .collect();

        proposals
    }

    /// Signs a proposal with the user's key.
    pub async fn sign_proposal(&mut self, proposal_id: &str) -> Result<Proposal> {
        let account = self.require_account()?;

        // Check if user is a cosigner
        let user_commitment = self.key_manager.commitment();
        if !account.is_cosigner(&user_commitment) {
            return Err(MultisigError::NotCosigner);
        }

        // Get the proposal to sign
        let proposals = self.list_proposals().await?;
        let proposal = proposals
            .iter()
            .find(|p| p.id == proposal_id)
            .ok_or_else(|| MultisigError::ProposalNotFound(proposal_id.to_string()))?;

        // Check if already signed
        if proposal.has_signed(&self.key_manager.commitment_hex()) {
            return Err(MultisigError::AlreadySigned);
        }

        // Sign the transaction summary commitment
        let tx_commitment = proposal.tx_summary.to_commitment();
        let signature_hex = self.key_manager.sign_hex(tx_commitment);

        // Build the ProposalSignature
        let signature = ProposalSignature::Falcon {
            signature: signature_hex,
        };

        let account_id = self.require_account()?.id();

        // Push signature to PSM
        let mut psm_client = self.create_authenticated_psm_client().await?;
        psm_client
            .sign_delta_proposal(&account_id, proposal_id, signature)
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to sign proposal: {}", e)))?;

        // Refresh and return updated proposal
        let proposals = self.list_proposals().await?;
        proposals
            .into_iter()
            .find(|p| p.id == proposal_id)
            .ok_or_else(|| MultisigError::ProposalNotFound(proposal_id.to_string()))
    }

    /// Executes a proposal when it has enough signatures.
    ///
    /// This will:
    /// 1. Sync with the Miden network to get latest chain state
    /// 2. Get the proposal and verify it has enough signatures
    /// 3. Push delta to PSM to get acknowledgment signature
    /// 4. Build the transaction with all cosigner signatures + PSM ack
    /// 5. Execute the transaction on-chain
    /// 6. Sync and update local account state
    pub async fn execute_proposal(&mut self, proposal_id: &str) -> Result<()> {
        // Sync with the network before executing to ensure we have latest state
        self.sync().await?;

        let account = self.require_account()?.clone();
        let account_id = account.id();

        // Get the raw proposal from PSM (need access to signatures)
        let mut psm_client = self.create_authenticated_psm_client().await?;
        let proposals_response = psm_client
            .get_delta_proposals(&account_id)
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to get proposals: {}", e)))?;

        // Find the proposal by ID
        let proposal = self
            .list_proposals()
            .await?
            .into_iter()
            .find(|p| p.id == proposal_id)
            .ok_or_else(|| MultisigError::ProposalNotFound(proposal_id.to_string()))?;

        // Verify proposal is ready (has enough signatures)
        if !proposal.status.is_ready() {
            let (collected, required) = proposal.signature_counts();
            return Err(MultisigError::ProposalNotReady {
                collected,
                required,
            });
        }

        // Find the raw delta object to get signatures
        let raw_proposal = proposals_response
            .proposals
            .iter()
            .find(|p| p.nonce == proposal.nonce)
            .ok_or_else(|| MultisigError::ProposalNotFound(proposal_id.to_string()))?;

        let tx_summary_commitment = proposal.tx_summary.to_commitment();

        // Collect signatures from the delta payload (available even after READY)
        let mut signature_inputs: Vec<SignatureInput> = {
            let payload_json: serde_json::Value = serde_json::from_str(&raw_proposal.delta_payload)
                .map_err(|e| {
                    MultisigError::MidenClient(format!(
                        "failed to parse delta payload signatures: {}",
                        e
                    ))
                })?;
            payload_json
                .get("signatures")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|sig| {
                            let signer = sig.get("signer_id")?.as_str()?;
                            let sig_hex = sig.get("signature")?.get("signature")?.as_str()?;
                            Some(SignatureInput {
                                signer_commitment: signer.to_string(),
                                signature_hex: sig_hex.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        };

        // Also collect any signatures present in pending status from PSM (if still pending)
        if let Some(ref status) = raw_proposal.status
            && let Some(ref status_oneof) = status.status
            && let Status::Pending(pending) = status_oneof
        {
            for cosigner_sig in &pending.cosigner_sigs {
                let sig_hex = cosigner_sig
                    .signature
                    .as_ref()
                    .ok_or_else(|| {
                        MultisigError::Signature(format!(
                            "missing signature for cosigner {}",
                            cosigner_sig.signer_id
                        ))
                    })?
                    .signature
                    .clone();
                signature_inputs.push(SignatureInput {
                    signer_commitment: cosigner_sig.signer_id.clone(),
                    signature_hex: sig_hex,
                });
            }
        }

        // Deduplicate by signer commitment
        signature_inputs.sort_by(|a, b| a.signer_commitment.cmp(&b.signer_commitment));
        signature_inputs.dedup_by(|a, b| a.signer_commitment == b.signer_commitment);

        // Build signature advice from cosigner signatures
        // Important: Use CURRENT account signers for validation, not proposal's new signers.
        // The on-chain MASM verifies signatures against the currently stored public keys.
        let required_commitments: HashSet<String> =
            account.cosigner_commitments_hex().into_iter().collect();
        let mut signature_advice = collect_signature_advice(
            signature_inputs,
            &required_commitments,
            tx_summary_commitment,
        )?;

        // SwitchPsm does NOT require PSM signature - skip push_delta for this transaction type
        let is_switch_psm = matches!(
            &proposal.transaction_type,
            TransactionType::SwitchPsm { .. }
        );

        if !is_switch_psm {
            // Get PSM ack signature and add to advice
            let psm_advice = self
                .get_psm_ack_signature(
                    &account,
                    proposal.nonce,
                    &proposal.tx_summary,
                    tx_summary_commitment,
                )
                .await?;
            signature_advice.push(psm_advice);
        }

        // Build the final transaction request with all signatures
        let salt = proposal.metadata.salt()?;

        // For signer-update transactions, we must propagate parse errors for signer commitments
        // rather than silently converting to None. This ensures malformed hex is diagnosed properly.
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

        // Execute and finalize
        self.finalize_transaction(account_id, final_tx_request, &proposal.transaction_type)
            .await
    }

    /// Creates a proposal for a transaction.
    ///
    /// This is the primary API for creating multisig transaction proposals.
    /// It handles all transaction types through a unified interface.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use miden_multisig_client::TransactionType;
    ///
    /// // Add a new cosigner
    /// let proposal = client.propose_transaction(
    ///     TransactionType::AddCosigner { new_commitment }
    /// ).await?;
    ///
    /// // Remove a cosigner
    /// let proposal = client.propose_transaction(
    ///     TransactionType::RemoveCosigner { commitment }
    /// ).await?;
    /// ```
    pub async fn propose_transaction(
        &mut self,
        transaction_type: TransactionType,
    ) -> Result<Proposal> {
        // Sync with the network before executing transaction
        self.sync().await?;

        let account = self.require_account()?.clone();
        let mut psm_client = self.create_authenticated_psm_client().await?;

        ProposalBuilder::new(transaction_type)
            .build(
                &mut self.miden_client,
                &mut psm_client,
                &account,
                self.key_manager.as_ref(),
            )
            .await
    }

    /// Proposes a transaction with automatic fallback to offline mode.
    ///
    /// First attempts to create the proposal via PSM. If PSM is unavailable
    /// (connection error), automatically falls back to offline proposal creation.
    ///
    /// This is useful when you want to attempt online coordination but have a
    /// graceful fallback path for offline sharing.
    ///
    /// # Returns
    ///
    /// - `ProposalResult::Online(Proposal)` if PSM succeeded
    /// - `ProposalResult::Offline(ExportedProposal)` if PSM failed
    ///
    /// # Example
    ///
    /// ```ignore
    /// use miden_multisig_client::{TransactionType, ProposalResult};
    ///
    /// let result = client.propose_with_fallback(
    ///     TransactionType::add_cosigner(new_commitment)
    /// ).await?;
    ///
    /// match result {
    ///     ProposalResult::Online(proposal) => {
    ///         println!("Proposal {} created on PSM", proposal.id);
    ///     }
    ///     ProposalResult::Offline(exported) => {
    ///         println!("PSM unavailable, share this file with cosigners:");
    ///         std::fs::write("proposal.json", exported.to_json()?)?;
    ///     }
    /// }
    /// ```
    pub async fn propose_with_fallback(
        &mut self,
        transaction_type: TransactionType,
    ) -> Result<ProposalResult> {
        // Try online first
        match self.propose_transaction(transaction_type.clone()).await {
            Ok(proposal) => Ok(ProposalResult::Online(Box::new(proposal))),
            Err(MultisigError::PsmConnection(_) | MultisigError::PsmServer(_)) => {
                // PSM unavailable, fall back to offline
                let exported = self.create_proposal_offline(transaction_type).await?;
                Ok(ProposalResult::Offline(Box::new(exported)))
            }
            Err(e) => Err(e),
        }
    }
}
