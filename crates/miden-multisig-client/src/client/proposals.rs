//! Proposal workflow operations for MultisigClient.
//!
//! This module handles listing, signing, executing, and creating proposals
//! via GUARDIAN (online mode).

use std::collections::HashSet;

use guardian_shared::{ProposalSignature, ToJson};
use miden_client::transaction::TransactionRequest;

use super::{MultisigClient, ProposalResult};
use crate::error::{MultisigError, Result};
use crate::execution::{
    SignatureAdvice, SignatureInput, build_final_transaction_request, collect_signature_advice,
};
use crate::keystore::proposal_public_key_hex;
use crate::proposal::{Proposal, TransactionType, is_builtin_proposal_type};
use crate::transaction::{
    ProposalBuilder, deserialize_transaction_request, execute_for_summary, word_to_hex,
};

impl MultisigClient {
    async fn get_proposal(
        &mut self,
        account_id: &miden_protocol::account::AccountId,
        proposal_id: &str,
    ) -> Result<Proposal> {
        let mut guardian_client = self.create_authenticated_guardian_client().await?;
        let response = guardian_client
            .get_delta_proposal(account_id, proposal_id)
            .await
            .map_err(|e| MultisigError::GuardianServer(format!("failed to get proposal: {}", e)))?;

        let raw_proposal = response
            .proposal
            .ok_or_else(|| MultisigError::ProposalNotFound(proposal_id.to_string()))?;
        Self::ensure_proposal_account_id(&raw_proposal.account_id, account_id)?;
        let proposal = Proposal::from(&raw_proposal)?;
        self.verify_proposal_summary_binding(&proposal).await?;
        Ok(proposal)
    }

    /// Lists pending proposals for the current account.
    ///
    /// # Errors
    ///
    /// Returns an error if any proposal from GUARDIAN cannot be parsed. This ensures
    /// malformed GUARDIAN payloads are surfaced rather than silently dropped.
    pub async fn list_proposals(&mut self) -> Result<Vec<Proposal>> {
        let account_id = self.require_account()?.id();

        let mut guardian_client = self.create_authenticated_guardian_client().await?;

        let response = guardian_client
            .get_delta_proposals(&account_id)
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!("failed to get proposals: {}", e))
            })?;

        let mut proposals = Vec::with_capacity(response.proposals.len());
        for delta in &response.proposals {
            Self::ensure_proposal_account_id(&delta.account_id, &account_id)?;
            let proposal = Proposal::from(delta)?;
            self.verify_proposal_summary_binding(&proposal).await?;
            proposals.push(proposal);
        }

        Ok(proposals)
    }

    /// Signs a proposal with the user's key.
    pub async fn sign_proposal(&mut self, proposal_id: &str) -> Result<Proposal> {
        let account = self.require_account()?;

        // Check if user is a cosigner
        let user_commitment = self.key_manager.commitment();
        if !account.is_cosigner(&user_commitment) {
            return Err(MultisigError::NotCosigner);
        }

        let account_id = account.id();
        let proposal = self.get_proposal(&account_id, proposal_id).await?;

        // Check if already signed
        if proposal.has_signed(&self.key_manager.commitment_hex()) {
            return Err(MultisigError::AlreadySigned);
        }

        // Sign the transaction summary commitment
        let tx_commitment = proposal.tx_summary.to_commitment();
        let signature_hex = self.key_manager.sign_word_hex(tx_commitment);

        // Build the ProposalSignature
        let signature = ProposalSignature::from_scheme(
            self.key_manager.scheme(),
            signature_hex,
            proposal_public_key_hex(self.key_manager.as_ref()),
        );

        // Push signature to GUARDIAN
        let mut guardian_client = self.create_authenticated_guardian_client().await?;
        let sign_response = guardian_client
            .sign_delta_proposal(&account_id, proposal_id, signature)
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!("failed to sign proposal: {}", e))
            })?;

        let updated_raw = sign_response
            .delta
            .as_ref()
            .ok_or_else(|| MultisigError::ProposalNotFound(proposal_id.to_string()))?;
        Self::ensure_proposal_account_id(&updated_raw.account_id, &account_id)?;
        let updated = Proposal::from(updated_raw)?;
        Ok(updated)
    }

    /// Executes a proposal when it has enough signatures.
    ///
    /// This will:
    /// 1. Sync with the Miden network to get latest chain state
    /// 2. Get the proposal and verify it has enough signatures
    /// 3. Push delta to GUARDIAN to get acknowledgment signature
    /// 4. Build the transaction with all cosigner signatures + GUARDIAN ack
    /// 5. Execute the transaction on-chain
    /// 6. Sync and update local account state
    pub async fn execute_proposal(&mut self, proposal_id: &str) -> Result<()> {
        // Sync with the network before executing to ensure we have latest state
        self.sync().await?;

        let account = self.require_account()?.clone();
        let account_id = account.id();

        let proposal = self.get_proposal(&account_id, proposal_id).await?;

        // Verify proposal is ready (has enough signatures)
        if !proposal.status.is_ready() {
            let (collected, required) = proposal.signature_counts();
            return Err(MultisigError::ProposalNotReady {
                collected,
                required,
            });
        }

        // Custom proposals (issue #266) have no per-type reconstruction recipe,
        // so the SDK cannot build/submit them. The integration executes them
        // with its own recipe using the advice from `prepare_custom_execution`.
        if matches!(proposal.transaction_type, TransactionType::Custom) {
            return Err(MultisigError::UnsupportedTransactionType(
                "custom proposals are executed by the integration; call \
                 prepare_custom_execution to get the cosigner + GUARDIAN advice"
                    .to_string(),
            ));
        }

        let tx_summary_commitment = proposal.tx_summary.to_commitment();

        let mut signature_inputs: Vec<SignatureInput> = proposal
            .signatures
            .into_iter()
            .map(|signature| SignatureInput {
                signer_commitment: signature.signer_commitment,
                signature_hex: signature.signature_hex,
                scheme: signature.scheme,
                public_key_hex: signature.public_key_hex,
            })
            .collect();

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

        if proposal.transaction_type.requires_guardian_ack() {
            // Get GUARDIAN ack signature and add to advice
            let guardian_advice = self
                .get_guardian_ack_signature(
                    &account,
                    proposal.nonce,
                    &proposal.tx_summary,
                    tx_summary_commitment,
                )
                .await?;
            signature_advice.push(guardian_advice);
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

        // Execute and finalize
        self.finalize_transaction(account_id, final_tx_request, &proposal.transaction_type)
            .await
    }

    /// Creates a proposal from a producer-built transaction the SDK does not
    /// model (issue #266 producer API). `transaction_request_bytes` is a serialized
    /// `TransactionRequest`; `proposal_type` is a free-form, non-empty label
    /// that MUST NOT collide with a built-in type. The integration keeps its own
    /// recipe to execute later via `prepare_custom_execution`.
    pub async fn propose_custom_transaction(
        &mut self,
        transaction_request_bytes: &[u8],
        proposal_type: &str,
    ) -> Result<Proposal> {
        let proposal_type = proposal_type.trim().to_lowercase();
        if proposal_type.is_empty() {
            return Err(MultisigError::InvalidConfig(
                "proposal_type must not be empty".to_string(),
            ));
        }
        if !proposal_type
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(MultisigError::InvalidConfig(format!(
                "proposal_type '{}' must be lowercase snake_case ([a-z0-9_]): no spaces, hyphens, or other characters",
                proposal_type
            )));
        }
        if is_builtin_proposal_type(&proposal_type) {
            return Err(MultisigError::UnsupportedTransactionType(format!(
                "'{}' is a built-in proposal type; use the typed proposal API instead",
                proposal_type
            )));
        }

        self.sync().await?;
        let account = self.require_account()?.clone();
        let account_id = account.id();

        let tx_request = deserialize_transaction_request(transaction_request_bytes)?;
        let tx_summary =
            execute_for_summary(&mut self.miden_client, account_id, tx_request).await?;
        let tx_commitment = tx_summary.to_commitment();

        let required_signatures = account.threshold()? as usize;

        let metadata = crate::proposal::ProposalMetadata {
            tx_summary_json: Some(tx_summary.to_json()),
            proposal_type: Some(proposal_type.to_string()),
            required_signatures: Some(required_signatures),
            signers: vec![self.key_manager.commitment_hex()],
            ..Default::default()
        };

        let payload = crate::payload::ProposalPayload::new(&tx_summary)
            .with_signature(self.key_manager.as_ref(), tx_commitment)
            .with_custom_metadata(proposal_type.to_string())
            .with_required_signatures(required_signatures);

        let nonce = account.nonce() + 1;
        let mut guardian_client = self.create_authenticated_guardian_client().await?;
        let response = guardian_client
            .push_delta_proposal(&account_id, nonce, &payload.to_json())
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!("failed to push proposal: {}", e))
            })?;

        let proposal = Proposal::new(tx_summary, nonce, TransactionType::Custom, metadata);

        if !proposal
            .id
            .trim_start_matches("0x")
            .eq_ignore_ascii_case(response.commitment.trim_start_matches("0x"))
        {
            return Err(MultisigError::GuardianServer(format!(
                "GUARDIAN returned proposal commitment {} but expected {}",
                response.commitment, proposal.id
            )));
        }

        Ok(proposal)
    }

    /// Assembles the validated execution advice for a threshold-met custom
    /// proposal (issue #266 producer API): the cosigner signatures and the
    /// GUARDIAN acknowledgment, keyed for the transaction's advice map. The
    /// integration injects this into its own rebuilt transaction request
    /// (`request.advice_map_mut().extend(advice)`) and submits it via its own
    /// Miden client.
    ///
    /// `transaction_request_bytes` (the serialized transaction request) is used only to verify,
    /// before the acknowledgment is requested, that it reproduces the signed
    /// proposal commitment. On a not-ready proposal or a binding mismatch this
    /// fails before requesting the acknowledgment.
    pub async fn prepare_custom_execution(
        &mut self,
        proposal_id: &str,
        transaction_request_bytes: &[u8],
    ) -> Result<Vec<SignatureAdvice>> {
        self.sync().await?;
        let account = self.require_account()?.clone();
        let account_id = account.id();

        let proposal = self.get_proposal(&account_id, proposal_id).await?;

        if !matches!(proposal.transaction_type, TransactionType::Custom) {
            return Err(MultisigError::UnsupportedTransactionType(
                "prepare_custom_execution is only for custom proposals; use execute_proposal \
                 for built-in types"
                    .to_string(),
            ));
        }

        if !proposal.status.is_ready() {
            let (collected, required) = proposal.signature_counts();
            return Err(MultisigError::ProposalNotReady {
                collected,
                required,
            });
        }

        let tx_summary_commitment = proposal.tx_summary.to_commitment();

        let probe_request = deserialize_transaction_request(transaction_request_bytes)?;
        let derived_summary =
            execute_for_summary(&mut self.miden_client, account_id, probe_request).await?;
        let derived_commitment = derived_summary.to_commitment();
        if derived_commitment != tx_summary_commitment {
            return Err(MultisigError::InvalidConfig(format!(
                "transaction request does not match the signed proposal commitment \
                 (expected {}, got {})",
                word_to_hex(&tx_summary_commitment),
                word_to_hex(&derived_commitment)
            )));
        }

        let mut signature_inputs: Vec<SignatureInput> = proposal
            .signatures
            .into_iter()
            .map(|signature| SignatureInput {
                signer_commitment: signature.signer_commitment,
                signature_hex: signature.signature_hex,
                scheme: signature.scheme,
                public_key_hex: signature.public_key_hex,
            })
            .collect();
        signature_inputs.sort_by(|a, b| a.signer_commitment.cmp(&b.signer_commitment));
        signature_inputs.dedup_by(|a, b| a.signer_commitment == b.signer_commitment);

        let required_commitments: HashSet<String> =
            account.cosigner_commitments_hex().into_iter().collect();
        let mut signature_advice = collect_signature_advice(
            signature_inputs,
            &required_commitments,
            tx_summary_commitment,
        )?;

        if proposal.transaction_type.requires_guardian_ack() {
            let guardian_advice = self
                .get_guardian_ack_signature(
                    &account,
                    proposal.nonce,
                    &derived_summary,
                    tx_summary_commitment,
                )
                .await?;
            signature_advice.push(guardian_advice);
        }

        Ok(signature_advice)
    }

    /// Submits an integration-built transaction on-chain (issue #266 producer
    /// API). The caller injects the advice from `prepare_custom_execution` into
    /// its own transaction request (`request.advice_map_mut().extend(advice)`)
    /// and passes it here to finalize.
    pub async fn submit_transaction(&mut self, request: TransactionRequest) -> Result<()> {
        // Refresh local state first: the account may have advanced between
        // `prepare_custom_execution` and submit, and submitting against stale
        // state would reject an otherwise-valid request.
        self.sync().await?;
        let account_id = self.require_account()?.id();
        self.miden_client
            .submit_new_transaction(account_id, request)
            .await
            .map_err(|e| {
                MultisigError::TransactionExecution(format!(
                    "transaction submission failed: {:?}",
                    e
                ))
            })?;
        let _ = self.miden_client.sync_state().await;
        Ok(())
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
        let mut guardian_client = self.create_authenticated_guardian_client().await?;

        ProposalBuilder::new(transaction_type)
            .build(
                &mut self.miden_client,
                &mut guardian_client,
                &account,
                self.key_manager.as_ref(),
            )
            .await
    }

    /// Proposes a transaction with automatic fallback to offline mode.
    ///
    /// First attempts to create the proposal via GUARDIAN. If GUARDIAN is unavailable
    /// (connection error), falls back to offline proposal creation only when
    /// the transaction supports GUARDIAN-less execution (`SwitchGuardian`).
    ///
    /// This is useful when you want to attempt online coordination but have a
    /// graceful fallback path for offline sharing.
    ///
    /// # Returns
    ///
    /// - `ProposalResult::Online(Proposal)` if GUARDIAN succeeded
    /// - `ProposalResult::Offline(ExportedProposal)` if GUARDIAN failed and transaction is `SwitchGuardian`
    ///
    /// # Example
    ///
    /// ```ignore
    /// use miden_multisig_client::{TransactionType, ProposalResult};
    ///
    /// let tx = TransactionType::switch_guardian("https://new-guardian.example.com", new_guardian_commitment);
    /// let result = client.propose_with_fallback(
    ///     tx
    /// ).await?;
    ///
    /// match result {
    ///     ProposalResult::Online(proposal) => {
    ///         println!("Proposal {} created on GUARDIAN", proposal.id);
    ///     }
    ///     ProposalResult::Offline(exported) => {
    ///         println!("GUARDIAN unavailable, share this file with cosigners:");
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
            Err(
                error @ (MultisigError::GuardianConnection(_) | MultisigError::GuardianServer(_)),
            ) => {
                if transaction_type.supports_offline_execution() {
                    let exported = self.create_proposal_offline(transaction_type).await?;
                    Ok(ProposalResult::Offline(Box::new(exported)))
                } else {
                    Err(error)
                }
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use guardian_client::DeltaObject;
    use guardian_shared::ToJson;
    use miden_protocol::account::AccountId;
    use miden_protocol::account::delta::{AccountDelta, AccountStorageDelta, AccountVaultDelta};
    use miden_protocol::transaction::{InputNotes, RawOutputNotes, TransactionSummary};
    use miden_protocol::{Felt, Word, ZERO};

    use crate::error::{MultisigError, Result};
    use crate::proposal::Proposal;

    fn create_test_tx_summary(account_id: &str, seed: u64) -> TransactionSummary {
        let account_id = AccountId::from_hex(account_id).expect("valid account id");
        let account_delta = AccountDelta::new(
            account_id,
            AccountStorageDelta::default(),
            AccountVaultDelta::default(),
            Felt::ZERO,
        )
        .expect("valid delta");

        TransactionSummary::new(
            account_delta,
            InputNotes::new(Vec::new()).expect("empty input notes"),
            RawOutputNotes::new(Vec::new()).expect("empty output notes"),
            Word::from([Felt::new(seed), ZERO, ZERO, ZERO]),
        )
    }

    fn proposal_delta(
        account_id: &str,
        nonce: u64,
        new_commitment: &str,
        seed: u64,
    ) -> DeltaObject {
        let payload = serde_json::json!({
            "tx_summary": create_test_tx_summary(account_id, seed).to_json(),
            "signatures": [],
            "metadata": {
                "proposal_type": "switch_guardian",
                "required_signatures": 1,
                "new_guardian_pubkey": "0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
                "new_guardian_endpoint": "http://new-guardian.example.com"
            }
        });

        DeltaObject {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: "0x000".to_string(),
            delta_payload: serde_json::to_string(&payload).expect("payload serialization"),
            new_commitment: new_commitment.to_string(),
            ack_sig: String::new(),
            ack_pubkey: None,
            ack_scheme: None,
            candidate_at: String::new(),
            canonical_at: None,
            discarded_at: None,
            status: None,
        }
    }

    #[test]
    fn inline_iteration_selects_by_unique_id_when_nonce_collides() {
        let same_nonce = 42;
        let delta_a = proposal_delta("0x7bfb0f38b0fafa103f86a805594170", same_nonce, "0xaaa", 1);
        let delta_b = proposal_delta("0x7bfb0f38b0fafa103f86a805594171", same_nonce, "0xbbb", 2);

        let target = Proposal::from(&delta_b).expect("proposal parses");

        let proposals = [delta_a, delta_b.clone()];
        let mut matched: Option<(&DeltaObject, Proposal)> = None;
        for raw_proposal in &proposals {
            let parsed = Proposal::from(raw_proposal).expect("parses");
            if parsed.id == target.id {
                matched = Some((raw_proposal, parsed));
            }
        }
        let (raw, parsed) = matched.expect("proposal should be found");

        assert_eq!(parsed.id, target.id);
        assert_eq!(parsed.nonce, same_nonce);
        assert_eq!(raw.new_commitment, delta_b.new_commitment);
    }

    #[test]
    fn inline_iteration_rejects_duplicate_ids() {
        let delta = proposal_delta("0x7bfb0f38b0fafa103f86a805594170", 42, "0xaaa", 1);
        let proposal_id = Proposal::from(&delta).expect("proposal parses").id;

        let mut matched: Option<(&DeltaObject, Proposal)> = None;
        let err = (&[delta.clone(), delta] as &[DeltaObject])
            .iter()
            .try_for_each(|raw_proposal| -> Result<()> {
                let parsed = Proposal::from(raw_proposal)?;
                if parsed.id == proposal_id {
                    if matched.is_some() {
                        return Err(MultisigError::InvalidConfig(format!(
                            "multiple proposals returned with the same ID {}",
                            proposal_id
                        )));
                    }
                    matched = Some((raw_proposal, parsed));
                }
                Ok(())
            })
            .expect_err("duplicate ids should fail");

        match err {
            MultisigError::InvalidConfig(message) => {
                assert!(message.contains("multiple proposals returned with the same ID"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
