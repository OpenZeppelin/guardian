//! Main MultisigClient implementation.

use std::collections::HashSet;

use base64::Engine;
use miden_client::account::Account;
use miden_client::note::NoteRelevance;
use miden_client::{Client, Deserializable, Serializable};
use miden_confidential_contracts::multisig_psm::{MultisigPsmBuilder, MultisigPsmConfig};
use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::account::auth::Signature as AccountSignature;
use miden_objects::asset::{Asset, FungibleAsset};
use miden_objects::crypto::dsa::rpo_falcon512::Signature as RpoFalconSignature;
use miden_objects::note::NoteId;
use private_state_manager_client::delta_status::Status;
use private_state_manager_client::{
    Auth, AuthConfig, FalconRpoSigner, MidenFalconRpoAuth, PsmClient, auth_config::AuthType,
};
use private_state_manager_shared::hex::FromHex;
use private_state_manager_shared::{ProposalSignature, ToJson};

use crate::account::MultisigAccount;
use crate::builder::MultisigClientBuilder;
use crate::error::{MultisigError, Result};
use crate::keystore::KeyManager;
use crate::proposal::{Proposal, ProposalStatus, TransactionType};
use crate::sync::sync_miden_state;
use crate::transaction::ProposalBuilder;

/// Main client for interacting with multisig accounts.
///
/// This client manages a single multisig account connected to a PSM server,
/// providing a high-level API for creating and managing multisig accounts,
/// proposals, and transactions.
///
/// # Example
///
/// ```ignore
/// use miden_multisig_client::{MultisigClient, MultisigConfig, PsmConfig};
/// use miden_client::rpc::Endpoint;
///
/// // Create a client
/// let mut client = MultisigClient::builder()
///     .miden_endpoint(Endpoint::new("http://localhost:57291"))
///     .psm_endpoint("http://localhost:50051")
///     .data_dir("/tmp/multisig")
///     .generate_key()
///     .build()
///     .await?;
///
/// // Create a multisig account
/// let account = client.create_account(2, vec![signer1, signer2]).await?;
/// ```
pub struct MultisigClient {
    miden_client: Client<()>,
    key_manager: Box<dyn KeyManager>,
    /// PSM server endpoint.
    psm_endpoint: String,
    /// The multisig account managed by this client.
    account: Option<MultisigAccount>,
}

impl MultisigClient {
    /// Creates a new MultisigClientBuilder.
    pub fn builder() -> MultisigClientBuilder {
        MultisigClientBuilder::new()
    }

    /// Creates a new MultisigClient (internal use, prefer builder).
    pub(crate) fn new(
        miden_client: Client<()>,
        key_manager: Box<dyn KeyManager>,
        psm_endpoint: String,
    ) -> Self {
        Self {
            miden_client,
            key_manager,
            psm_endpoint,
            account: None,
        }
    }

    /// Returns the PSM endpoint.
    pub fn psm_endpoint(&self) -> &str {
        &self.psm_endpoint
    }

    /// Returns the current account, if any.
    pub fn account(&self) -> Option<&MultisigAccount> {
        self.account.as_ref()
    }

    /// Returns the current account ID, if any.
    pub fn account_id(&self) -> Option<AccountId> {
        self.account.as_ref().map(|a| a.id())
    }

    /// Returns true if an account is loaded.
    pub fn has_account(&self) -> bool {
        self.account.is_some()
    }

    /// Returns the user's public key commitment as a Word.
    pub fn user_commitment(&self) -> Word {
        self.key_manager.commitment()
    }

    /// Returns the user's public key commitment as a hex string.
    pub fn user_commitment_hex(&self) -> String {
        self.key_manager.commitment_hex()
    }

    /// Returns a reference to the key manager.
    pub fn key_manager(&self) -> &dyn KeyManager {
        self.key_manager.as_ref()
    }

    /// Creates a PSM client (unauthenticated).
    async fn create_psm_client(&self) -> Result<PsmClient> {
        PsmClient::connect(&self.psm_endpoint)
            .await
            .map_err(|e| MultisigError::PsmConnection(e.to_string()))
    }

    /// Creates an authenticated PSM client.
    async fn create_authenticated_psm_client(&self) -> Result<PsmClient> {
        let client = self.create_psm_client().await?;

        // Create Auth from our key manager's secret key
        let secret_key = self.key_manager.clone_secret_key();
        let signer = FalconRpoSigner::new(secret_key);
        let auth = Auth::FalconRpoSigner(signer);

        Ok(client.with_auth(auth))
    }

    /// Creates a new multisig account.
    ///
    /// This will:
    /// 1. Fetch the PSM server's public key commitment
    /// 2. Create the multisig account using miden-confidential-contracts
    /// 3. Add the account to the local miden-client
    /// 4. Store the account in the client
    ///
    /// Note: After creation, you should call `push_account` to register
    /// the account with the PSM server.
    pub async fn create_account(
        &mut self,
        threshold: u32,
        signer_commitments: Vec<Word>,
    ) -> Result<&MultisigAccount> {
        // Get PSM server's public key commitment
        let mut psm_client = self.create_psm_client().await?;
        let psm_pubkey_hex = psm_client
            .get_pubkey()
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to get PSM pubkey: {}", e)))?;

        let psm_commitment = crate::keystore::commitment_from_hex(&psm_pubkey_hex)
            .map_err(MultisigError::HexDecode)?;

        // Create the multisig account
        let psm_config = MultisigPsmConfig::new(threshold, signer_commitments, psm_commitment);

        // Generate a random seed for account ID
        let mut seed = [0u8; 32];
        rand::Rng::fill(&mut rand::rng(), &mut seed);

        let account = MultisigPsmBuilder::new(psm_config)
            .with_seed(seed)
            .build()
            .map_err(|e| MultisigError::MidenClient(format!("failed to build account: {}", e)))?;

        // Add to miden-client
        self.miden_client
            .add_account(&account, false)
            .await
            .map_err(|e| MultisigError::MidenClient(format!("failed to add account: {}", e)))?;

        // Wrap in MultisigAccount and store
        let multisig_account = MultisigAccount::new(account, &self.psm_endpoint);
        self.account = Some(multisig_account);

        Ok(self.account.as_ref().unwrap())
    }

    /// Pulls an account from PSM and loads it locally.
    ///
    /// Use this when joining an existing multisig as a cosigner.
    pub async fn pull_account(&mut self, account_id: AccountId) -> Result<&MultisigAccount> {
        let mut psm_client = self.create_authenticated_psm_client().await?;

        let state_response = psm_client
            .get_state(&account_id)
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to get state: {}", e)))?;

        // Extract state JSON from response
        let state_obj = state_response
            .state
            .ok_or_else(|| MultisigError::PsmServer("no state returned from PSM".to_string()))?;

        // Parse the state JSON to get the base64-encoded account
        let state_value: serde_json::Value = serde_json::from_str(&state_obj.state_json)?;

        let account_base64 = state_value["data"]
            .as_str()
            .ok_or_else(|| MultisigError::PsmServer("missing 'data' field in state".to_string()))?;

        let account_bytes = base64::engine::general_purpose::STANDARD
            .decode(account_base64)
            .map_err(|e| MultisigError::MidenClient(format!("failed to decode account: {}", e)))?;

        let account = Account::read_from_bytes(&account_bytes).map_err(|e| {
            MultisigError::MidenClient(format!("failed to deserialize account: {}", e))
        })?;

        // Add to miden-client
        self.miden_client
            .add_account(&account, true) // true = imported
            .await
            .map_err(|e| MultisigError::MidenClient(format!("failed to add account: {}", e)))?;

        // Wrap and store
        let multisig_account = MultisigAccount::new(account, &self.psm_endpoint);
        self.account = Some(multisig_account);

        Ok(self.account.as_ref().unwrap())
    }

    /// Pushes the current account to PSM for initial registration.
    ///
    /// This should be called after `create_account` to register the account
    /// with the PSM server so other cosigners can pull it.
    pub async fn push_account(&mut self) -> Result<()> {
        let account = self
            .account
            .as_ref()
            .ok_or_else(|| MultisigError::MissingConfig("no account loaded".to_string()))?;

        // Use authenticated client for PSM configuration
        let mut psm_client = self.create_authenticated_psm_client().await?;

        // Serialize account to base64 (matching the demo pattern)
        let account_bytes = account.inner().to_bytes();
        let account_base64 = base64::engine::general_purpose::STANDARD.encode(&account_bytes);

        let initial_state = serde_json::json!({
            "data": account_base64,
            "account_id": account.id().to_string(),
        });

        // Build auth config with cosigner commitments
        let cosigner_commitments = account.cosigner_commitments_hex();
        let auth_config = AuthConfig {
            auth_type: Some(AuthType::MidenFalconRpo(MidenFalconRpoAuth {
                cosigner_commitments,
            })),
        };

        let account_id = account.id();

        // Configure account on PSM
        psm_client
            .configure(&account_id, auth_config, initial_state, "Filesystem")
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to configure account: {}", e)))?;

        Ok(())
    }

    /// Returns a reference to the current account, or error if none loaded.
    fn require_account(&self) -> Result<&MultisigAccount> {
        self.account
            .as_ref()
            .ok_or_else(|| MultisigError::MissingConfig("no account loaded".to_string()))
    }

    /// Lists pending proposals for the current account.
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

        let proposals = response
            .proposals
            .iter()
            .filter_map(|delta| Proposal::from(delta, current_threshold, &current_signers).ok())
            .collect();

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
    /// 1. Get the proposal and verify it has enough signatures
    /// 2. Push delta to PSM to get acknowledgment signature
    /// 3. Build the transaction with all cosigner signatures + PSM ack
    /// 4. Execute the transaction on-chain
    /// 5. Sync and update local account state
    pub async fn execute_proposal(&mut self, proposal_id: &str) -> Result<()> {
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
            let (collected, required) = match &proposal.status {
                ProposalStatus::Pending {
                    signatures_collected,
                    signatures_required,
                    ..
                } => (*signatures_collected, *signatures_required),
                _ => (0, 0),
            };
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

        // Build signature advice from cosigner signatures
        // Important: Use CURRENT account signers for validation, not proposal's new signers.
        // The on-chain MASM verifies signatures against the currently stored public keys.
        let mut signature_advice = Vec::new();
        let required_commitments: HashSet<String> =
            account.cosigner_commitments_hex().into_iter().collect();
        let mut added_signers: HashSet<String> = HashSet::new();

        if let Some(ref status) = raw_proposal.status
            && let Some(ref status_oneof) = status.status
            && let Status::Pending(pending) = status_oneof
        {
            for cosigner_sig in pending.cosigner_sigs.iter() {
                let sig_hex = cosigner_sig
                    .signature
                    .as_ref()
                    .ok_or_else(|| MultisigError::Signature("missing signature".to_string()))?
                    .signature
                    .as_str();

                // Only include signatures from required signers
                if !required_commitments
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(&cosigner_sig.signer_id))
                {
                    continue;
                }

                // Skip duplicates
                if !added_signers.insert(cosigner_sig.signer_id.clone()) {
                    continue;
                }

                let sig = RpoFalconSignature::from_hex(sig_hex).map_err(|e| {
                    MultisigError::Signature(format!("invalid cosigner signature: {}", e))
                })?;

                let commitment = crate::keystore::commitment_from_hex(&cosigner_sig.signer_id)
                    .map_err(MultisigError::HexDecode)?;

                signature_advice.push(crate::transaction::build_signature_advice_entry(
                    commitment,
                    tx_summary_commitment,
                    &AccountSignature::from(sig),
                ));
            }
        }

        // SwitchPsm does NOT require PSM signature - skip push_delta for this transaction type
        let is_switch_psm = matches!(
            &proposal.transaction_type,
            TransactionType::SwitchPsm { .. }
        );

        if !is_switch_psm {
            // Get current account commitment
            let prev_commitment = format!("0x{}", hex::encode(account.commitment().as_bytes()));

            // Push delta to PSM to get acknowledgment signature
            let mut psm_client = self.create_authenticated_psm_client().await?;
            let delta_payload = proposal.tx_summary.to_json();

            let push_response = psm_client
                .push_delta(
                    &account_id,
                    proposal.nonce,
                    &prev_commitment,
                    &delta_payload,
                )
                .await
                .map_err(|e| MultisigError::PsmServer(format!("failed to push delta: {}", e)))?;

            // Get PSM ack signature
            let ack_sig = push_response.ack_sig.ok_or_else(|| {
                MultisigError::PsmServer("PSM did not return acknowledgment signature".to_string())
            })?;

            // Get PSM's pubkey commitment
            let psm_commitment_hex = psm_client.get_pubkey().await.map_err(|e| {
                MultisigError::PsmServer(format!("failed to get PSM commitment: {}", e))
            })?;

            // Add 0x prefix if needed
            let ack_sig_with_prefix = if ack_sig.starts_with("0x") {
                ack_sig.clone()
            } else {
                format!("0x{}", ack_sig)
            };

            let ack_signature =
                RpoFalconSignature::from_hex(&ack_sig_with_prefix).map_err(|e| {
                    MultisigError::Signature(format!("failed to parse PSM ack signature: {}", e))
                })?;

            let psm_commitment = crate::keystore::commitment_from_hex(&psm_commitment_hex)
                .map_err(MultisigError::HexDecode)?;

            signature_advice.push(crate::transaction::build_signature_advice_entry(
                psm_commitment,
                tx_summary_commitment,
                &AccountSignature::from(ack_signature),
            ));
        }

        // Build the final transaction request with all signatures
        let salt = proposal.metadata.salt()?;

        let final_tx_request = match &proposal.transaction_type {
            TransactionType::P2ID {
                recipient,
                faucet_id,
                amount,
            } => {
                let asset = FungibleAsset::new(*faucet_id, *amount).map_err(|e| {
                    MultisigError::InvalidConfig(format!("failed to create asset: {}", e))
                })?;

                crate::transaction::build_p2id_transaction_request(
                    account.inner(),
                    *recipient,
                    vec![asset.into()],
                    salt,
                    signature_advice,
                )?
            }
            TransactionType::ConsumeNotes { note_ids } => {
                crate::transaction::build_consume_notes_transaction_request(
                    note_ids.clone(),
                    salt,
                    signature_advice,
                )?
            }
            TransactionType::SwitchPsm { new_commitment, .. } => {
                crate::transaction::build_update_psm_transaction_request(
                    *new_commitment,
                    salt,
                    signature_advice,
                )?
            }
            _ => {
                // Signer update transactions (AddCosigner, RemoveCosigner, UpdateSigners)
                let signer_commitments = proposal.metadata.signer_commitments()?;
                let new_threshold = proposal
                    .metadata
                    .new_threshold
                    .ok_or_else(|| MultisigError::MissingConfig("new_threshold".to_string()))?;

                let (tx_request, _) = crate::transaction::build_update_signers_transaction_request(
                    new_threshold,
                    &signer_commitments,
                    salt,
                    signature_advice,
                )?;

                tx_request
            }
        };

        // Capture the new PSM endpoint if this is a SwitchPsm transaction
        let new_psm_endpoint =
            if let TransactionType::SwitchPsm { new_endpoint, .. } = &proposal.transaction_type {
                Some(new_endpoint.clone())
            } else {
                None
            };

        // Execute the transaction on-chain
        self.miden_client
            .submit_new_transaction(account_id, final_tx_request)
            .await
            .map_err(|e| {
                MultisigError::TransactionExecution(format!(
                    "transaction execution failed: {:?}",
                    e
                ))
            })?;

        // Sync with network to get the updated account state
        self.sync().await?;

        // Update local account cache from miden-client
        let account_record = self
            .miden_client
            .get_account(account_id)
            .await
            .map_err(|e| {
                MultisigError::MidenClient(format!("failed to get updated account: {}", e))
            })?
            .ok_or_else(|| {
                MultisigError::MissingConfig("account not found after sync".to_string())
            })?;

        let updated_account: Account = account_record.into();

        // Update PSM endpoint if this was a SwitchPsm transaction, then register on new PSM
        if let Some(endpoint) = new_psm_endpoint {
            self.psm_endpoint = endpoint;

            // Update local account with new PSM endpoint
            let multisig_account =
                MultisigAccount::new(updated_account.clone(), &self.psm_endpoint);
            self.account = Some(multisig_account);

            // Register the updated account on the new PSM server
            self.push_account().await.map_err(|e| {
                MultisigError::PsmServer(format!(
                    "transaction executed successfully but failed to register on new PSM: {}",
                    e
                ))
            })?;
        } else {
            let multisig_account = MultisigAccount::new(updated_account, &self.psm_endpoint);
            self.account = Some(multisig_account);
        }

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
    ) -> Result<crate::export::ExportedProposal> {
        use crate::export::{
            EXPORT_VERSION, ExportedMetadata, ExportedProposal, ExportedSignature,
        };
        use miden_objects::asset::FungibleAsset;
        use private_state_manager_shared::ToJson;

        // Sync with the network before executing transaction
        self.sync().await?;

        let account = self.require_account()?.clone();
        let account_id = account.id();
        let current_threshold = account.threshold()?;

        // Generate salt for replay protection
        let salt = crate::transaction::generate_salt();
        let salt_hex = crate::transaction::word_to_hex(&salt);

        // Build transaction request based on type
        let (tx_request, metadata) = match &transaction_type {
            TransactionType::SwitchPsm {
                new_endpoint,
                new_commitment,
            } => {
                let tx_request = crate::transaction::build_update_psm_transaction_request(
                    *new_commitment,
                    salt,
                    std::iter::empty(),
                )?;

                let metadata = ExportedMetadata {
                    salt_hex: Some(salt_hex.clone()),
                    new_psm_pubkey_hex: Some(crate::transaction::word_to_hex(new_commitment)),
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

                let tx_request = crate::transaction::build_p2id_transaction_request(
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
                let tx_request = crate::transaction::build_consume_notes_transaction_request(
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

                let (tx_request, _) = crate::transaction::build_update_signers_transaction_request(
                    new_threshold,
                    &current_signers,
                    salt,
                    std::iter::empty(),
                )?;

                let signer_commitments_hex: Vec<String> = current_signers
                    .iter()
                    .map(crate::transaction::word_to_hex)
                    .collect();

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

                let (tx_request, _) = crate::transaction::build_update_signers_transaction_request(
                    new_threshold,
                    &new_signers,
                    salt,
                    std::iter::empty(),
                )?;

                let signer_commitments_hex: Vec<String> = new_signers
                    .iter()
                    .map(crate::transaction::word_to_hex)
                    .collect();

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
                let (tx_request, _) = crate::transaction::build_update_signers_transaction_request(
                    *new_threshold as u64,
                    signer_commitments,
                    salt,
                    std::iter::empty(),
                )?;

                let signer_commitments_hex: Vec<String> = signer_commitments
                    .iter()
                    .map(crate::transaction::word_to_hex)
                    .collect();

                let metadata = ExportedMetadata {
                    salt_hex: Some(salt_hex.clone()),
                    new_threshold: Some(*new_threshold as u64),
                    signer_commitments_hex,
                    ..Default::default()
                };

                (tx_request, metadata)
            }
            TransactionType::Unknown => {
                return Err(MultisigError::InvalidConfig(
                    "Unknown transaction type".to_string(),
                ));
            }
        };

        // Execute to get the TransactionSummary
        let tx_summary =
            crate::transaction::execute_for_summary(&mut self.miden_client, account_id, tx_request)
                .await?;

        // Sign the transaction summary commitment
        let tx_commitment = tx_summary.to_commitment();
        let signature_hex = self.key_manager.sign_hex(tx_commitment);

        // Build the proposal ID from commitment
        let id = format!(
            "0x{}",
            hex::encode(
                tx_commitment
                    .iter()
                    .flat_map(|f| f.as_int().to_le_bytes())
                    .collect::<Vec<_>>()
            )
        );

        // Determine transaction type string
        let tx_type_str = match &transaction_type {
            TransactionType::P2ID { .. } => "P2ID",
            TransactionType::ConsumeNotes { .. } => "ConsumeNotes",
            TransactionType::AddCosigner { .. } => "AddCosigner",
            TransactionType::RemoveCosigner { .. } => "RemoveCosigner",
            TransactionType::SwitchPsm { .. } => "SwitchPsm",
            TransactionType::UpdateSigners { .. } => "UpdateSigners",
            TransactionType::Unknown => "Unknown",
        };

        // Create exported proposal with our signature
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
            }],
            signatures_required: current_threshold as usize,
            metadata,
        };

        Ok(exported)
    }

    /// Syncs state with the Miden network.
    pub async fn sync(&mut self) -> Result<()> {
        sync_miden_state(&mut self.miden_client).await
    }

    /// Syncs account state from PSM and updates the local cache.
    pub async fn sync_account(&mut self) -> Result<()> {
        let account_id = self.require_account()?.id();
        self.pull_account(account_id).await?;
        Ok(())
    }

    /// Registers the current account on the PSM server.
    ///
    /// This is useful after:
    /// - Switching to a new PSM endpoint
    /// - Re-registering an account that was removed from PSM
    /// - Initial account setup (alternative to the automatic registration in `create_account`)
    ///
    /// The account must already be loaded locally via `create_account` or `pull_account`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // After switching PSM endpoints
    /// client.set_psm_endpoint("http://new-psm:50051");
    /// client.register_on_psm().await?;
    /// ```
    pub async fn register_on_psm(&mut self) -> Result<()> {
        self.push_account().await
    }

    /// Changes the PSM endpoint and optionally registers the account on the new server.
    ///
    /// Use this when:
    /// - The PSM server has moved to a new address (same server, new URL)
    /// - You want to switch to a different PSM provider without on-chain changes
    ///
    /// **Note:** This does NOT update the on-chain PSM public key. For that, use
    /// `propose_transaction(TransactionType::SwitchPsm { ... })` which will:
    /// 1. Update the PSM public key on-chain
    /// 2. Execute the transaction
    /// 3. Automatically register on the new PSM
    ///
    /// # Arguments
    ///
    /// * `new_endpoint` - The new PSM server endpoint URL
    /// * `register` - If true, registers the current account on the new PSM server
    ///
    /// # Example
    ///
    /// ```ignore
    /// // PSM server moved to new URL (same keys, no on-chain change needed)
    /// client.set_psm_endpoint("http://new-psm:50051", true).await?;
    /// ```
    pub async fn set_psm_endpoint(&mut self, new_endpoint: &str, register: bool) -> Result<()> {
        self.psm_endpoint = new_endpoint.to_string();

        // Update the account's PSM endpoint reference
        if let Some(account) = self.account.take() {
            let updated = MultisigAccount::new(account.into_inner(), &self.psm_endpoint);
            self.account = Some(updated);
        }

        if register {
            self.register_on_psm().await?;
        }

        Ok(())
    }

    /// Lists notes that can be consumed by the current account.
    ///
    /// Returns a list of notes that are committed on-chain and can be consumed
    /// immediately by the multisig account.
    pub async fn list_consumable_notes(&mut self) -> Result<Vec<ConsumableNote>> {
        let account_id = self.require_account()?.id();

        // Sync first to get latest notes
        self.sync().await?;

        let consumable = self
            .miden_client
            .get_consumable_notes(Some(account_id))
            .await
            .map_err(|e| {
                MultisigError::MidenClient(format!("failed to get consumable notes: {}", e))
            })?;

        // Convert to our wrapper type, filtering for notes consumable "Now"
        let notes = consumable
            .into_iter()
            .filter_map(|(record, relevances)| {
                // Only include notes consumable "Now" by our account
                let can_consume_now = relevances
                    .iter()
                    .any(|(id, rel)| *id == account_id && matches!(rel, NoteRelevance::Now));
                if can_consume_now {
                    Some(ConsumableNote {
                        id: record.id(),
                        assets: record.assets().iter().cloned().collect(),
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(notes)
    }

    /// Returns a list of all committed notes (not just consumable).
    pub async fn list_committed_notes(&mut self) -> Result<Vec<ConsumableNote>> {
        let account_id = self.require_account()?.id();

        // Sync first to get latest notes
        self.sync().await?;

        let notes = self
            .miden_client
            .get_consumable_notes(Some(account_id))
            .await
            .map_err(|e| MultisigError::MidenClient(format!("failed to get notes: {}", e)))?;

        let result = notes
            .into_iter()
            .filter(|(_, relevances)| relevances.iter().any(|(id, _)| *id == account_id))
            .map(|(record, _)| ConsumableNote {
                id: record.id(),
                assets: record.assets().iter().cloned().collect(),
            })
            .collect();

        Ok(result)
    }

    // ==================== Export/Import Methods ====================

    /// Exports a proposal to a file for offline sharing.
    ///
    /// This fetches the proposal from PSM, including all collected signatures,
    /// and writes it to the specified file path as JSON.
    ///
    /// # Example
    ///
    /// ```ignore
    /// client.export_proposal(&proposal_id, "/tmp/proposal.json").await?;
    /// ```
    pub async fn export_proposal(
        &mut self,
        proposal_id: &str,
        path: &std::path::Path,
    ) -> Result<()> {
        let exported = self.export_proposal_to_exported(proposal_id).await?;
        let json = exported.to_json()?;
        std::fs::write(path, json)
            .map_err(|e| MultisigError::InvalidConfig(format!("failed to write file: {}", e)))?;
        Ok(())
    }

    /// Exports a proposal to a JSON string for programmatic use.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let json = client.export_proposal_to_string(&proposal_id).await?;
    /// println!("{}", json);
    /// ```
    pub async fn export_proposal_to_string(&mut self, proposal_id: &str) -> Result<String> {
        let exported = self.export_proposal_to_exported(proposal_id).await?;
        exported.to_json()
    }

    /// Internal helper to create an ExportedProposal from PSM data.
    async fn export_proposal_to_exported(
        &mut self,
        proposal_id: &str,
    ) -> Result<crate::export::ExportedProposal> {
        use crate::export::{ExportedProposal, ExportedSignature};

        let account = self.require_account()?.clone();
        let account_id = account.id();

        // Get the proposal
        let proposals = self.list_proposals().await?;
        let proposal = proposals
            .iter()
            .find(|p| p.id == proposal_id)
            .ok_or_else(|| MultisigError::ProposalNotFound(proposal_id.to_string()))?;

        // Get raw delta to extract signatures
        let mut psm_client = self.create_authenticated_psm_client().await?;
        let proposals_response = psm_client
            .get_delta_proposals(&account_id)
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to get proposals: {}", e)))?;

        // Find the raw proposal and extract signatures
        let raw_proposal = proposals_response
            .proposals
            .iter()
            .find(|p| p.nonce == proposal.nonce);

        let mut signatures = Vec::new();
        if let Some(raw) = raw_proposal
            && let Some(ref status) = raw.status
            && let Some(ref status_oneof) = status.status
            && let Status::Pending(pending) = status_oneof
        {
            for cosigner_sig in pending.cosigner_sigs.iter() {
                if let Some(ref sig) = cosigner_sig.signature {
                    signatures.push(ExportedSignature {
                        signer_commitment: cosigner_sig.signer_id.clone(),
                        signature: sig.signature.clone(),
                    });
                }
            }
        }

        let exported =
            ExportedProposal::from_proposal(proposal, account_id).with_signatures(signatures);

        Ok(exported)
    }

    /// Imports a proposal from a file.
    ///
    /// The proposal can then be signed with `sign_imported_proposal`
    /// or executed with `execute_imported_proposal`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let proposal = client.import_proposal("/tmp/proposal.json")?;
    /// println!("Imported proposal: {}", proposal.id);
    /// ```
    pub fn import_proposal(
        &self,
        path: &std::path::Path,
    ) -> Result<crate::export::ExportedProposal> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| MultisigError::InvalidConfig(format!("failed to read file: {}", e)))?;
        self.import_proposal_from_string(&json)
    }

    /// Imports a proposal from a JSON string.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let proposal = client.import_proposal_from_string(&json)?;
    /// ```
    pub fn import_proposal_from_string(
        &self,
        json: &str,
    ) -> Result<crate::export::ExportedProposal> {
        use crate::export::ExportedProposal;

        let exported = ExportedProposal::from_json(json)?;

        // Validate account ID matches if we have an account loaded
        if let Some(account) = &self.account {
            let expected_id = account.id().to_string();
            if !exported.account_id.eq_ignore_ascii_case(&expected_id) {
                return Err(MultisigError::InvalidConfig(format!(
                    "proposal account {} does not match loaded account {}",
                    exported.account_id, expected_id
                )));
            }
        }

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
    pub fn sign_imported_proposal(
        &self,
        proposal: &mut crate::export::ExportedProposal,
    ) -> Result<()> {
        use crate::export::ExportedSignature;
        use miden_objects::transaction::TransactionSummary;
        use private_state_manager_shared::FromJson;

        let account = self.require_account()?;

        // Check if user is a cosigner
        let user_commitment = self.key_manager.commitment();
        if !account.is_cosigner(&user_commitment) {
            return Err(MultisigError::NotCosigner);
        }

        // Check if already signed
        let user_commitment_hex = self.key_manager.commitment_hex();
        if proposal.signatures.iter().any(|s| {
            s.signer_commitment
                .eq_ignore_ascii_case(&user_commitment_hex)
        }) {
            return Err(MultisigError::AlreadySigned);
        }

        // Parse the transaction summary to get the commitment
        let tx_summary = TransactionSummary::from_json(&proposal.tx_summary).map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to parse tx_summary: {}", e))
        })?;

        // Sign the transaction summary commitment
        let tx_commitment = tx_summary.to_commitment();
        let signature_hex = self.key_manager.sign_hex(tx_commitment);

        // Add signature to proposal
        proposal.add_signature(ExportedSignature {
            signer_commitment: user_commitment_hex,
            signature: signature_hex,
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
    pub async fn execute_imported_proposal(
        &mut self,
        exported: &crate::export::ExportedProposal,
    ) -> Result<()> {
        use miden_objects::account::auth::Signature as AccountSignature;
        use miden_objects::asset::FungibleAsset;
        use miden_objects::crypto::dsa::rpo_falcon512::Signature as RpoFalconSignature;
        use miden_objects::transaction::TransactionSummary;
        use private_state_manager_shared::FromJson;
        use private_state_manager_shared::hex::FromHex;

        let account = self.require_account()?.clone();
        let account_id = account.id();

        // Verify proposal is ready
        if !exported.is_ready() {
            return Err(MultisigError::ProposalNotReady {
                collected: exported.signatures_collected(),
                required: exported.signatures_required,
            });
        }

        // Parse the proposal
        let proposal = exported.to_proposal()?;
        let tx_summary = TransactionSummary::from_json(&exported.tx_summary).map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to parse tx_summary: {}", e))
        })?;
        let tx_summary_commitment = tx_summary.to_commitment();

        // Build signature advice from exported signatures
        let required_commitments: std::collections::HashSet<String> =
            account.cosigner_commitments_hex().into_iter().collect();
        let mut signature_advice = Vec::new();

        for sig in &exported.signatures {
            // Only include signatures from required signers
            if !required_commitments
                .iter()
                .any(|c| c.eq_ignore_ascii_case(&sig.signer_commitment))
            {
                continue;
            }

            let sig_hex = if sig.signature.starts_with("0x") {
                sig.signature.clone()
            } else {
                format!("0x{}", sig.signature)
            };

            let rpo_sig = RpoFalconSignature::from_hex(&sig_hex)
                .map_err(|e| MultisigError::Signature(format!("invalid signature: {}", e)))?;

            let commitment = crate::keystore::commitment_from_hex(&sig.signer_commitment)
                .map_err(MultisigError::HexDecode)?;

            signature_advice.push(crate::transaction::build_signature_advice_entry(
                commitment,
                tx_summary_commitment,
                &AccountSignature::from(rpo_sig),
            ));
        }

        // SwitchPsm does NOT require PSM signature
        let is_switch_psm = matches!(
            &proposal.transaction_type,
            TransactionType::SwitchPsm { .. }
        );

        if !is_switch_psm {
            // For offline execution, we need PSM ack signature
            // Get current account commitment
            let prev_commitment = format!("0x{}", hex::encode(account.commitment().as_bytes()));

            // Push delta to PSM to get acknowledgment signature
            let mut psm_client = self.create_authenticated_psm_client().await?;
            let delta_payload = tx_summary.to_json();

            let push_response = psm_client
                .push_delta(
                    &account_id,
                    proposal.nonce,
                    &prev_commitment,
                    &delta_payload,
                )
                .await
                .map_err(|e| MultisigError::PsmServer(format!("failed to push delta: {}", e)))?;

            // Get PSM ack signature
            let ack_sig = push_response.ack_sig.ok_or_else(|| {
                MultisigError::PsmServer("PSM did not return acknowledgment signature".to_string())
            })?;

            // Get PSM's pubkey commitment
            let psm_commitment_hex = psm_client.get_pubkey().await.map_err(|e| {
                MultisigError::PsmServer(format!("failed to get PSM commitment: {}", e))
            })?;

            let ack_sig_with_prefix = if ack_sig.starts_with("0x") {
                ack_sig.clone()
            } else {
                format!("0x{}", ack_sig)
            };

            let ack_signature =
                RpoFalconSignature::from_hex(&ack_sig_with_prefix).map_err(|e| {
                    MultisigError::Signature(format!("failed to parse PSM ack signature: {}", e))
                })?;

            let psm_commitment = crate::keystore::commitment_from_hex(&psm_commitment_hex)
                .map_err(MultisigError::HexDecode)?;

            signature_advice.push(crate::transaction::build_signature_advice_entry(
                psm_commitment,
                tx_summary_commitment,
                &AccountSignature::from(ack_signature),
            ));
        }

        // Build the final transaction request with all signatures
        let salt = proposal.metadata.salt()?;

        let final_tx_request = match &proposal.transaction_type {
            TransactionType::P2ID {
                recipient,
                faucet_id,
                amount,
            } => {
                let asset = FungibleAsset::new(*faucet_id, *amount).map_err(|e| {
                    MultisigError::InvalidConfig(format!("failed to create asset: {}", e))
                })?;

                crate::transaction::build_p2id_transaction_request(
                    account.inner(),
                    *recipient,
                    vec![asset.into()],
                    salt,
                    signature_advice,
                )?
            }
            TransactionType::ConsumeNotes { note_ids } => {
                crate::transaction::build_consume_notes_transaction_request(
                    note_ids.clone(),
                    salt,
                    signature_advice,
                )?
            }
            TransactionType::SwitchPsm { new_commitment, .. } => {
                crate::transaction::build_update_psm_transaction_request(
                    *new_commitment,
                    salt,
                    signature_advice,
                )?
            }
            _ => {
                // Signer update transactions
                let signer_commitments = proposal.metadata.signer_commitments()?;
                let new_threshold = proposal
                    .metadata
                    .new_threshold
                    .ok_or_else(|| MultisigError::MissingConfig("new_threshold".to_string()))?;

                let (tx_request, _) = crate::transaction::build_update_signers_transaction_request(
                    new_threshold,
                    &signer_commitments,
                    salt,
                    signature_advice,
                )?;

                tx_request
            }
        };

        // Capture the new PSM endpoint if this is a SwitchPsm transaction
        let new_psm_endpoint =
            if let TransactionType::SwitchPsm { new_endpoint, .. } = &proposal.transaction_type {
                Some(new_endpoint.clone())
            } else {
                None
            };

        // Execute the transaction on-chain
        self.miden_client
            .submit_new_transaction(account_id, final_tx_request)
            .await
            .map_err(|e| {
                MultisigError::TransactionExecution(format!(
                    "transaction execution failed: {:?}",
                    e
                ))
            })?;

        // Sync with network to get the updated account state
        self.sync().await?;

        // Update local account cache from miden-client
        let account_record = self
            .miden_client
            .get_account(account_id)
            .await
            .map_err(|e| {
                MultisigError::MidenClient(format!("failed to get updated account: {}", e))
            })?
            .ok_or_else(|| {
                MultisigError::MissingConfig("account not found after sync".to_string())
            })?;

        let updated_account: Account = account_record.into();

        // Update PSM endpoint if this was a SwitchPsm transaction
        if let Some(endpoint) = new_psm_endpoint {
            self.psm_endpoint = endpoint;
            let multisig_account =
                MultisigAccount::new(updated_account.clone(), &self.psm_endpoint);
            self.account = Some(multisig_account);

            // Register the updated account on the new PSM server
            self.push_account().await.map_err(|e| {
                MultisigError::PsmServer(format!(
                    "transaction executed successfully but failed to register on new PSM: {}",
                    e
                ))
            })?;
        } else {
            let multisig_account = MultisigAccount::new(updated_account, &self.psm_endpoint);
            self.account = Some(multisig_account);
        }

        Ok(())
    }
}

/// A wrapper type for a consumable note with simplified information.
#[derive(Debug, Clone)]
pub struct ConsumableNote {
    /// The note ID.
    pub id: NoteId,
    /// Assets contained in the note.
    pub assets: Vec<Asset>,
}
