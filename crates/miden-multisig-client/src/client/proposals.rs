//! Proposal workflow operations for MultisigClient.
//!
//! This module handles listing, signing, executing, and creating proposals
//! via PSM (online mode).

use std::collections::HashSet;

use private_state_manager_client::delta_status::Status;
use private_state_manager_shared::{DeltaPayload, ProposalSignature, SignatureScheme};

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

        let signature = ProposalSignature::from_scheme(self.key_manager.scheme(), signature_hex);

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
    /// 1. Sync with the Miden network to get latest chain state
    /// 2. Get the proposal and verify it has enough signatures
    /// 3. Push delta to PSM to get acknowledgment signature
    /// 4. Build the transaction with all cosigner signatures + PSM ack
    /// 5. Execute the transaction on-chain
    /// 6. Sync and update local account state
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

        let mut signature_inputs: Vec<SignatureInput> = {
            let payload: DeltaPayload =
                serde_json::from_str(&raw_proposal.delta_payload).map_err(|e| {
                    MultisigError::MidenClient(format!(
                        "failed to parse delta payload signatures: {}",
                        e
                    ))
                })?;
            payload
                .signatures
                .iter()
                .map(|ds| {
                    let (scheme, sig_hex, pk_hex) = match &ds.signature {
                        ProposalSignature::Falcon { signature } => {
                            (SignatureScheme::Falcon, signature.clone(), None)
                        }
                        ProposalSignature::Ecdsa {
                            signature,
                            public_key,
                        } => (
                            SignatureScheme::Ecdsa,
                            signature.clone(),
                            public_key.clone(),
                        ),
                    };
                    SignatureInput {
                        signer_commitment: ds.signer_id.clone(),
                        signature_hex: sig_hex,
                        scheme,
                        public_key_hex: pk_hex,
                    }
                })
                .collect()
        };

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
                let scheme_str = cosigner_sig
                    .signature
                    .as_ref()
                    .map(|s| s.scheme.as_str())
                    .unwrap_or("falcon");
                let scheme = match scheme_str {
                    "ecdsa" => SignatureScheme::Ecdsa,
                    _ => SignatureScheme::Falcon,
                };
                signature_inputs.push(SignatureInput {
                    signer_commitment: cosigner_sig.signer_id.clone(),
                    signature_hex: sig_hex,
                    scheme,
                    public_key_hex: None,
                });
            }
        }

        signature_inputs.sort_by(|a, b| a.signer_commitment.cmp(&b.signer_commitment));
        signature_inputs.dedup_by(|a, b| a.signer_commitment == b.signer_commitment);

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
