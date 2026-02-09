//! Proposal workflow operations for MultisigClient.
//!
//! This module handles listing, signing, executing, and creating proposals
//! via PSM (online mode).

use private_state_manager_shared::ProposalSignature;

use super::proposal::execution::signer_commitments_for_transaction;
use super::proposal::parser::{parse_unique_signature_inputs, required_commitments};
use super::{MultisigClient, ProposalResult};
use crate::error::{MultisigError, Result};
use crate::execution::{build_final_transaction_request, collect_signature_advice};
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

        let user_commitment = self.key_manager.commitment();
        if !account.is_cosigner(&user_commitment) {
            return Err(MultisigError::NotCosigner);
        }

        let proposals = self.list_proposals().await?;
        let proposal = proposals
            .iter()
            .find(|p| p.id == proposal_id)
            .ok_or_else(|| MultisigError::ProposalNotFound(proposal_id.to_string()))?;

        if proposal.has_signed(&self.key_manager.commitment_hex()) {
            return Err(MultisigError::AlreadySigned);
        }

        let tx_commitment = proposal.tx_summary.to_commitment();
        let signature_hex = self.key_manager.sign_hex(tx_commitment);

        let signature = ProposalSignature::from_scheme(
            self.key_manager.scheme(),
            signature_hex,
            self.key_manager.public_key_hex(),
        );

        let account_id = self.require_account()?.id();

        let mut psm_client = self.create_authenticated_psm_client().await?;
        psm_client
            .sign_delta_proposal(&account_id, proposal_id, signature)
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to sign proposal: {}", e)))?;

        let proposals = self.list_proposals().await?;
        proposals
            .into_iter()
            .find(|p| p.id == proposal_id)
            .ok_or_else(|| MultisigError::ProposalNotFound(proposal_id.to_string()))
    }

    /// Executes a proposal when it has enough signatures.
    ///
    /// This will:
    pub async fn execute_proposal(&mut self, proposal_id: &str) -> Result<()> {
        self.sync().await?;

        let account = self.require_account()?.clone();
        let account_id = account.id();

        let mut psm_client = self.create_authenticated_psm_client().await?;
        let proposals_response = psm_client
            .get_delta_proposals(&account_id)
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to get proposals: {}", e)))?;

        let proposal = self
            .list_proposals()
            .await?
            .into_iter()
            .find(|p| p.id == proposal_id)
            .ok_or_else(|| MultisigError::ProposalNotFound(proposal_id.to_string()))?;

        if !proposal.status.is_ready() {
            let (collected, required) = proposal.signature_counts();
            return Err(MultisigError::ProposalNotReady {
                collected,
                required,
            });
        }

        let raw_proposal = proposals_response
            .proposals
            .iter()
            .find(|p| p.nonce == proposal.nonce)
            .ok_or_else(|| MultisigError::ProposalNotFound(proposal_id.to_string()))?;

        let tx_summary_commitment = proposal.tx_summary.to_commitment();

        let signature_inputs = parse_unique_signature_inputs(&raw_proposal.delta_payload)?;
        let required_commitments = required_commitments(&account);
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
                .get_psm_ack_signature(
                    &account,
                    proposal.nonce,
                    &proposal.tx_summary,
                    tx_summary_commitment,
                )
                .await?;
            signature_advice.push(psm_advice);
        }

        let salt = proposal.metadata.salt()?;
        let signer_commitments = signer_commitments_for_transaction(&proposal)?;

        let final_tx_request = build_final_transaction_request(
            &self.miden_client,
            &proposal.transaction_type,
            account.inner(),
            salt,
            signature_advice,
            proposal.metadata.new_threshold,
            signer_commitments.as_deref(),
            self.key_manager.scheme(),
        )
        .await?;

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
    ///
    /// let proposal = client.propose_transaction(
    ///     TransactionType::AddCosigner { new_commitment }
    /// ).await?;
    ///
    ///
    /// let proposal = client.propose_transaction(
    ///     TransactionType::RemoveCosigner { commitment }
    /// ).await?;
    /// ```
    pub async fn propose_transaction(
        &mut self,
        transaction_type: TransactionType,
    ) -> Result<Proposal> {
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
        match self.propose_transaction(transaction_type.clone()).await {
            Ok(proposal) => Ok(ProposalResult::Online(Box::new(proposal))),
            Err(MultisigError::PsmConnection(_) | MultisigError::PsmServer(_)) => {
                let exported = self.create_proposal_offline(transaction_type).await?;
                Ok(ProposalResult::Offline(Box::new(exported)))
            }
            Err(e) => Err(e),
        }
    }
}
