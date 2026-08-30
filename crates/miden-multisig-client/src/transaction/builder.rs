//! Proposal builder for multisig transactions.

use guardian_client::GuardianClient;
use guardian_shared::{SignatureScheme, ToJson};
use miden_client::account::Account;
use miden_client::transaction::TransactionRequest;
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::asset::Asset;
use miden_protocol::note::{Note, NoteId, NoteType};

use crate::MidenSdkClient;
use crate::account::MultisigAccount;
use crate::error::{MultisigError, Result};
use crate::execution::build_transfer_asset;
use crate::guardian_endpoint::verify_endpoint_commitment;
use crate::keystore::{KeyManager, ensure_hex_prefix};
use crate::payload::ProposalPayload;
use crate::procedures::ProcedureName;
use crate::proposal::{P2ideHeights, Proposal, ProposalMetadata, TransactionType};
use crate::utils::hex_body_eq;

use super::{
    build_p2id_transaction_request, build_update_guardian_transaction_request,
    build_update_procedure_threshold_transaction_request, build_update_signers_transaction_request,
    chain_anchor_to_base64, execute_for_summary, generate_salt, word_to_hex,
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
                note_type,
                heights,
            } => {
                self.build_p2id(
                    miden_client,
                    guardian_client,
                    account,
                    recipient,
                    faucet_id,
                    amount,
                    note_type,
                    heights,
                    key_manager,
                )
                .await
            }
            TransactionType::ConsumeNotes { ref note_ids, .. } => {
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
            TransactionType::Custom => Err(MultisigError::UnsupportedTransactionType(
                "cannot create a proposal for a custom transaction type".to_string(),
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

    /// The signer-update request an `add_cosigner` proposal is created from.
    ///
    /// Every create path resolves its conversion info from the block the client is synced to,
    /// which is the block [`execute_for_summary`] will anchor the request at. Committing it is
    /// what lets the auth procedure's `fee::pay_fee` run, and resolving it here rather than at
    /// the chain tip on some later rebuild is what keeps the anchor and the committed faucet
    /// from disagreeing.
    async fn add_cosigner_request(
        miden_client: &MidenSdkClient,
        new_threshold: u64,
        signers: &[Word],
        salt: Word,
        scheme: SignatureScheme,
    ) -> Result<TransactionRequest> {
        let (tx_request, _config_hash) = build_update_signers_transaction_request(
            new_threshold,
            signers,
            salt,
            std::iter::empty(),
            scheme,
            Some(crate::execution::resolve_fee_conversion_info(miden_client).await?),
        )?;

        Ok(tx_request)
    }

    /// The signer-update request a `remove_cosigner` proposal is created from.
    async fn remove_cosigner_request(
        miden_client: &MidenSdkClient,
        new_threshold: u64,
        signers: &[Word],
        salt: Word,
        scheme: SignatureScheme,
    ) -> Result<TransactionRequest> {
        let (tx_request, _config_hash) = build_update_signers_transaction_request(
            new_threshold,
            signers,
            salt,
            std::iter::empty(),
            scheme,
            Some(crate::execution::resolve_fee_conversion_info(miden_client).await?),
        )?;

        Ok(tx_request)
    }

    /// The payment request a `p2id` proposal is created from.
    async fn p2id_request(
        miden_client: &MidenSdkClient,
        account: &Account,
        recipient: AccountId,
        assets: Vec<Asset>,
        note_type: NoteType,
        heights: P2ideHeights,
        salt: Word,
    ) -> Result<TransactionRequest> {
        build_p2id_transaction_request(
            account,
            recipient,
            assets,
            note_type,
            heights,
            salt,
            std::iter::empty(),
            Some(crate::execution::resolve_fee_conversion_info(miden_client).await?),
        )
    }

    /// The consumption request a `consume_notes` proposal is created from.
    async fn consume_notes_request(
        miden_client: &MidenSdkClient,
        notes: Vec<Note>,
        salt: Word,
    ) -> Result<TransactionRequest> {
        crate::transaction::build_consume_notes_transaction_request_from_notes(
            notes,
            salt,
            std::iter::empty(),
            Some(crate::execution::resolve_fee_conversion_info(miden_client).await?),
        )
    }

    /// The rotation request a `switch_guardian` proposal is created from.
    async fn switch_guardian_request(
        miden_client: &MidenSdkClient,
        new_guardian_pubkey: Word,
        scheme: SignatureScheme,
        salt: Word,
    ) -> Result<TransactionRequest> {
        build_update_guardian_transaction_request(
            new_guardian_pubkey,
            scheme,
            salt,
            std::iter::empty(),
            Some(crate::execution::resolve_fee_conversion_info(miden_client).await?),
        )
    }

    /// The threshold request an `update_procedure_threshold` proposal is created from.
    async fn update_procedure_threshold_request(
        miden_client: &MidenSdkClient,
        procedure: ProcedureName,
        new_threshold: u32,
        salt: Word,
    ) -> Result<TransactionRequest> {
        build_update_procedure_threshold_transaction_request(
            procedure,
            new_threshold,
            salt,
            std::iter::empty(),
            Some(crate::execution::resolve_fee_conversion_info(miden_client).await?),
        )
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
        let tx_request = Self::add_cosigner_request(
            miden_client,
            new_threshold,
            &current_signers,
            salt,
            key_manager.scheme(),
        )
        .await?;

        // Execute to get the TransactionSummary
        let (tx_summary, chain_anchor) =
            execute_for_summary(miden_client, account_id, tx_request).await?;

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
            note_type: None,
            reclaim_height: None,
            timelock_height: None,
            note_ids_hex: Vec::new(),
            consume_notes_metadata_version: None,
            consume_notes_notes: Vec::new(),
            new_guardian_pubkey_hex: None,
            new_guardian_endpoint: None,
            target_procedure: None,
            required_signatures: Some(required_signatures),
            signers: vec![key_manager.commitment_hex()],
            chain_anchor_b64: Some(chain_anchor_to_base64(&chain_anchor)),
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_add_signer_metadata(
                new_threshold,
                signer_commitments_hex.clone(),
                word_to_hex(&salt),
            )
            .with_required_signatures(required_signatures)
            .with_chain_anchor(chain_anchor_to_base64(&chain_anchor));

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
        let tx_request = Self::remove_cosigner_request(
            miden_client,
            new_threshold,
            &new_signers,
            salt,
            key_manager.scheme(),
        )
        .await?;

        // Execute to get the TransactionSummary
        let (tx_summary, chain_anchor) =
            execute_for_summary(miden_client, account_id, tx_request).await?;

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
            note_type: None,
            reclaim_height: None,
            timelock_height: None,
            note_ids_hex: Vec::new(),
            consume_notes_metadata_version: None,
            consume_notes_notes: Vec::new(),
            new_guardian_pubkey_hex: None,
            new_guardian_endpoint: None,
            target_procedure: None,
            required_signatures: Some(required_signatures),
            signers: vec![key_manager.commitment_hex()],
            chain_anchor_b64: Some(chain_anchor_to_base64(&chain_anchor)),
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_remove_signer_metadata(
                new_threshold,
                signer_commitments_hex.clone(),
                word_to_hex(&salt),
            )
            .with_required_signatures(required_signatures)
            .with_chain_anchor(chain_anchor_to_base64(&chain_anchor));

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
        note_type: NoteType,
        heights: P2ideHeights,
        key_manager: &dyn KeyManager,
    ) -> Result<Proposal> {
        let account_id = account.id();
        let required_signatures =
            account.effective_threshold_for_procedure(ProcedureName::SendAsset)? as usize;

        let asset = build_transfer_asset(faucet_id, amount)?;

        // Generate salt for replay protection
        let salt = generate_salt();

        // Build the P2ID transaction request (no signature advice needed for proposal)
        let tx_request = Self::p2id_request(
            miden_client,
            account.inner(),
            recipient,
            vec![asset.into()],
            note_type,
            heights,
            salt,
        )
        .await?;

        // Execute to get the TransactionSummary
        let (tx_summary, chain_anchor) =
            execute_for_summary(miden_client, account_id, tx_request).await?;

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
            note_type: (note_type != NoteType::Public).then(|| note_type.to_string()),
            reclaim_height: heights.reclaim,
            timelock_height: heights.timelock,
            note_ids_hex: Vec::new(),
            consume_notes_metadata_version: None,
            consume_notes_notes: Vec::new(),
            new_guardian_pubkey_hex: None,
            new_guardian_endpoint: None,
            target_procedure: None,
            required_signatures: Some(required_signatures),
            signers: vec![key_manager.commitment_hex()],
            chain_anchor_b64: Some(chain_anchor_to_base64(&chain_anchor)),
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_payment_metadata(
                recipient.to_string(),
                faucet_id.to_string(),
                amount,
                word_to_hex(&salt),
                note_type,
                heights,
            )
            .with_required_signatures(required_signatures)
            .with_chain_anchor(chain_anchor_to_base64(&chain_anchor));

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
                note_type,
                heights,
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

        // Fetch notes from the proposer's local store for v2 embedding (FR-012).
        let fetched_notes =
            crate::transaction::consume::fetch_notes_from_store(miden_client, &note_ids).await?;
        let serialized_notes: Vec<crate::proposal::SerializedNote> = fetched_notes
            .iter()
            .map(crate::proposal::SerializedNote::from_note)
            .collect();

        let tx_request = Self::consume_notes_request(miden_client, fetched_notes, salt).await?;

        let (tx_summary, chain_anchor) =
            execute_for_summary(miden_client, account_id, tx_request).await?;
        let tx_commitment = tx_summary.to_commitment();

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
            note_type: None,
            reclaim_height: None,
            timelock_height: None,
            note_ids_hex: note_ids_hex.clone(),
            consume_notes_metadata_version: Some(
                crate::proposal::CONSUME_NOTES_METADATA_VERSION_V2,
            ),
            consume_notes_notes: serialized_notes.clone(),
            new_guardian_pubkey_hex: None,
            new_guardian_endpoint: None,
            target_procedure: None,
            required_signatures: Some(required_signatures),
            signers: vec![key_manager.commitment_hex()],
            chain_anchor_b64: Some(chain_anchor_to_base64(&chain_anchor)),
        };

        let notes_base64: Vec<String> = serialized_notes
            .into_iter()
            .map(crate::proposal::SerializedNote::into_inner)
            .collect();

        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_note_consumption_metadata_v2(note_ids_hex, notes_base64, word_to_hex(&salt))
            .with_required_signatures(required_signatures)
            .with_chain_anchor(chain_anchor_to_base64(&chain_anchor));

        // FR-011: cap covers only the metadata fragment, not the full payload.
        if let Some(meta) = payload.metadata.as_ref() {
            let serialized_len = serde_json::to_vec(meta)
                .map_err(MultisigError::Serialization)?
                .len();
            if serialized_len > crate::proposal::MAX_CONSUME_NOTES_METADATA_BYTES {
                return Err(MultisigError::ConsumeNotesMetadataOversize {
                    limit: crate::proposal::MAX_CONSUME_NOTES_METADATA_BYTES,
                    actual: serialized_len,
                });
            }
        }

        // Push proposal to GUARDIAN
        let nonce = account.nonce() + 1;
        let response = guardian_client
            .push_delta_proposal(&account_id, nonce, &payload.to_json())
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!("failed to push proposal: {}", e))
            })?;

        // Clone notes for the runtime TransactionType; metadata keeps its own copy.
        let notes_for_tx_type = metadata.consume_notes_notes.clone();
        let proposal = Proposal::new(
            tx_summary,
            nonce,
            TransactionType::ConsumeNotes {
                note_ids,
                metadata_version: Some(crate::proposal::CONSUME_NOTES_METADATA_VERSION_V2),
                notes: notes_for_tx_type,
            },
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
        let tx_request = Self::switch_guardian_request(
            miden_client,
            new_guardian_pubkey,
            key_manager.scheme(),
            salt,
        )
        .await?;

        // Execute to get the TransactionSummary
        let (tx_summary, chain_anchor) =
            execute_for_summary(miden_client, account_id, tx_request).await?;

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
            note_type: None,
            reclaim_height: None,
            timelock_height: None,
            note_ids_hex: Vec::new(),
            consume_notes_metadata_version: None,
            consume_notes_notes: Vec::new(),
            new_guardian_pubkey_hex: Some(word_to_hex(&new_guardian_pubkey)),
            new_guardian_endpoint: Some(new_guardian_endpoint.clone()),
            target_procedure: None,
            required_signatures: Some(required_signatures),
            signers: vec![key_manager.commitment_hex()],
            chain_anchor_b64: Some(chain_anchor_to_base64(&chain_anchor)),
        };

        // Build the payload using ProposalPayload
        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_guardian_update_metadata(
                word_to_hex(&new_guardian_pubkey),
                new_guardian_endpoint.clone(),
                word_to_hex(&salt),
            )
            .with_required_signatures(required_signatures)
            .with_chain_anchor(chain_anchor_to_base64(&chain_anchor));

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
        let tx_request =
            Self::update_procedure_threshold_request(miden_client, procedure, new_threshold, salt)
                .await?;
        let (tx_summary, chain_anchor) =
            execute_for_summary(miden_client, account_id, tx_request).await?;
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
            note_type: None,
            reclaim_height: None,
            timelock_height: None,
            note_ids_hex: Vec::new(),
            consume_notes_metadata_version: None,
            consume_notes_notes: Vec::new(),
            new_guardian_pubkey_hex: None,
            new_guardian_endpoint: None,
            target_procedure: Some(procedure.to_string()),
            required_signatures: Some(required_signatures),
            signers: vec![key_manager.commitment_hex()],
            chain_anchor_b64: Some(chain_anchor_to_base64(&chain_anchor)),
        };

        let payload = ProposalPayload::new(&tx_summary)
            .with_signature(key_manager, tx_commitment)
            .with_procedure_threshold_metadata(procedure, new_threshold as u64, word_to_hex(&salt))
            .with_required_signatures(required_signatures)
            .with_chain_anchor(chain_anchor_to_base64(&chain_anchor));

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
    use miden_protocol::account::AccountStoragePatch;
    use miden_protocol::account::delta::{AccountDelta, AccountVaultDelta};
    use miden_protocol::transaction::{
        InputNotes, RawOutputNotes, TransactionSummary, TransactionSummaryUserParams,
    };
    use miden_protocol::{Felt, ZERO};

    fn test_proposal() -> Proposal {
        let account_id =
            AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").expect("valid account id");
        let account_delta = AccountDelta::new(
            account_id,
            AccountStoragePatch::default(),
            AccountVaultDelta::default(),
            None,
            Felt::ZERO,
        )
        .expect("valid delta");
        let tx_summary = TransactionSummary::new(
            account_delta,
            InputNotes::new(Vec::new()).expect("empty input notes"),
            RawOutputNotes::new(Vec::new()).expect("empty output notes"),
            Word::default(),
            0,
            TransactionSummaryUserParams::new([
                ZERO,
                ZERO,
                ZERO,
                Felt::new_unchecked(9),
                ZERO,
                ZERO,
                ZERO,
            ]),
        );

        Proposal::new(
            tx_summary,
            1,
            TransactionType::ConsumeNotes {
                note_ids: vec![miden_protocol::note::NoteId::from_raw(Word::from([
                    Felt::new_unchecked(1),
                    ZERO,
                    ZERO,
                    ZERO,
                ]))],
                metadata_version: None,
                notes: Vec::new(),
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

    mod fee_conversion_info {
        //! Every typed create path must commit the chain's own fee conversion info.
        //!
        //! The primitive is covered where it is applied (`transaction::mod`, `payment`), but
        //! those callers pass an explicit `Some(info)`, so they say nothing about whether a real
        //! create path passes one at all. These cases are what makes the *decision* observable:
        //! dropping the conversion info on any single path leaves a proposal whose auth arg the
        //! guarded auth procedure rejects at `fee::pay_fee`, and only the path's own case sees
        //! it. They mirror the ten TypeScript cases around `commits the chain fee faucet on a
        //! typed p2id proposal` in `packages/miden-multisig-client/src/multisig.test.ts`.

        use super::*;
        use crate::client::test_support::{
            assert_commits_fee_faucet, chain_fee_faucet, client_at_fee_faucet,
            guarded_multisig_account, p2id_note_for, test_wallet,
        };
        use miden_protocol::asset::FungibleAsset;

        fn salt() -> Word {
            Word::from([1u32, 2, 3, 4])
        }

        #[tokio::test]
        async fn add_cosigner_request_commits_the_chain_fee_faucet() {
            let dir = tempfile::tempdir().expect("temp dir");
            let client = client_at_fee_faucet(dir.path(), chain_fee_faucet()).await;

            let request = ProposalBuilder::add_cosigner_request(
                &client.miden_client,
                1,
                &[Word::from([5u32, 6, 7, 8])],
                salt(),
                SignatureScheme::Falcon,
            )
            .await
            .expect("the add-cosigner request builds");

            assert_commits_fee_faucet(&request, chain_fee_faucet(), salt());
        }

        #[tokio::test]
        async fn remove_cosigner_request_commits_the_chain_fee_faucet() {
            let dir = tempfile::tempdir().expect("temp dir");
            let client = client_at_fee_faucet(dir.path(), chain_fee_faucet()).await;

            let request = ProposalBuilder::remove_cosigner_request(
                &client.miden_client,
                1,
                &[Word::from([5u32, 6, 7, 8])],
                salt(),
                SignatureScheme::Falcon,
            )
            .await
            .expect("the remove-cosigner request builds");

            assert_commits_fee_faucet(&request, chain_fee_faucet(), salt());
        }

        #[tokio::test]
        async fn p2id_request_commits_the_chain_fee_faucet() {
            let dir = tempfile::tempdir().expect("temp dir");
            let client = client_at_fee_faucet(dir.path(), chain_fee_faucet()).await;
            let recipient = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b")
                .expect("valid recipient id");

            let request = ProposalBuilder::p2id_request(
                &client.miden_client,
                &guarded_multisig_account(),
                recipient,
                vec![FungibleAsset::mock(100)],
                NoteType::Public,
                P2ideHeights::default(),
                salt(),
            )
            .await
            .expect("the p2id request builds");

            assert_commits_fee_faucet(&request, chain_fee_faucet(), salt());
        }

        #[tokio::test]
        async fn consume_notes_request_commits_the_chain_fee_faucet() {
            let dir = tempfile::tempdir().expect("temp dir");
            let client = client_at_fee_faucet(dir.path(), chain_fee_faucet()).await;
            let note = p2id_note_for(&test_wallet(1), 1, NoteType::Public);

            let request =
                ProposalBuilder::consume_notes_request(&client.miden_client, vec![note], salt())
                    .await
                    .expect("the consume-notes request builds");

            assert_commits_fee_faucet(&request, chain_fee_faucet(), salt());
        }

        #[tokio::test]
        async fn switch_guardian_request_commits_the_chain_fee_faucet() {
            let dir = tempfile::tempdir().expect("temp dir");
            let client = client_at_fee_faucet(dir.path(), chain_fee_faucet()).await;

            let request = ProposalBuilder::switch_guardian_request(
                &client.miden_client,
                Word::from([1u32, 1, 1, 1]),
                SignatureScheme::Falcon,
                salt(),
            )
            .await
            .expect("the switch-guardian request builds");

            assert_commits_fee_faucet(&request, chain_fee_faucet(), salt());
        }

        #[tokio::test]
        async fn update_procedure_threshold_request_commits_the_chain_fee_faucet() {
            let dir = tempfile::tempdir().expect("temp dir");
            let client = client_at_fee_faucet(dir.path(), chain_fee_faucet()).await;

            let request = ProposalBuilder::update_procedure_threshold_request(
                &client.miden_client,
                ProcedureName::SendAsset,
                2,
                salt(),
            )
            .await
            .expect("the procedure-threshold request builds");

            assert_commits_fee_faucet(&request, chain_fee_faucet(), salt());
        }
    }
}
