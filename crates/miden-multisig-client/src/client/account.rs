//! Account lifecycle operations for MultisigClient.
//!
//! This module handles account creation, pulling/pushing from GUARDIAN,
//! syncing, and registration operations.

use std::collections::HashSet;

use base64::Engine;
use guardian_client::{GuardianClient, TryIntoTxSummary};
use guardian_shared::SignatureScheme;
use guardian_shared::retry::retry_delay;
use miden_client::account::Account;
use miden_client::note::{NoteFile, NoteSyncHint};
use miden_client::store::{InputNoteState, NoteFilter, TransactionFilter};
use miden_client::transaction::TransactionStatus;
use miden_client::{Deserializable, Serializable};
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::Word;
use miden_protocol::account::AccountId;

use super::{MultisigClient, StateVerificationResult};
use crate::account::MultisigAccount;
use crate::error::{MultisigError, Result};
use crate::keystore::word_from_hex;
use crate::procedures::ProcedureThreshold;
use crate::signers::SignerSpec;
use crate::transaction::word_to_hex;

impl MultisigClient {
    fn ensure_unique_signer_commitments(signer_commitments: &[Word]) -> Result<()> {
        let mut seen = HashSet::new();

        for commitment in signer_commitments {
            let commitment_hex = word_to_hex(commitment);
            if !seen.insert(commitment_hex.clone()) {
                return Err(MultisigError::InvalidConfig(format!(
                    "duplicate signer commitment: {}",
                    commitment_hex
                )));
            }
        }

        Ok(())
    }

    /// Refuses to list this client's own key under a scheme other than its
    /// own: a key's commitment verifies only under the scheme that derived
    /// it, so the creator would be locked out of the account it creates.
    fn ensure_own_key_scheme(&self, signers: &[SignerSpec]) -> Result<()> {
        let own = self.key_manager.commitment();
        let own_scheme = self.key_manager.scheme();
        match signers
            .iter()
            .find(|signer| signer.commitment == own && signer.scheme != own_scheme)
        {
            Some(signer) => Err(MultisigError::InvalidConfig(format!(
                "signer {} is this client's {own_scheme} key and cannot be listed under {}",
                word_to_hex(&own),
                signer.scheme
            ))),
            None => Ok(()),
        }
    }

    /// Creates a new multisig account.
    ///
    /// On a fee-charging network, fund the new account with the native fee asset before creating
    /// regular proposals. A `consume_notes` proposal can consume the funding note and pay its own
    /// fee.
    ///
    /// # Arguments
    /// * `threshold` - Minimum number of signatures required (default threshold)
    /// * `signer_commitments` - Public key commitments of all signers
    ///
    /// For per-procedure thresholds, use `create_account_with_proc_thresholds` instead.
    pub async fn create_account(
        &mut self,
        threshold: u32,
        signer_commitments: Vec<Word>,
    ) -> Result<&MultisigAccount> {
        self.create_account_with_proc_thresholds(threshold, signer_commitments, Vec::new())
            .await
    }

    /// Creates a new multisig account with per-procedure threshold overrides.
    ///
    /// On a fee-charging network, fund the new account with the native fee asset before creating
    /// regular proposals. A `consume_notes` proposal can consume the funding note and pay its own
    /// fee.
    ///
    /// # Arguments
    /// * `threshold` - Minimum number of signatures required (default threshold)
    /// * `signer_commitments` - Public key commitments of all signers
    /// * `proc_threshold_overrides` - Per-procedure threshold overrides using named procedures.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use miden_multisig_client::{ProcedureThreshold, ProcedureName};
    ///
    /// let thresholds = vec![
    ///     ProcedureThreshold::new(ProcedureName::ReceiveAsset, 1),
    ///     ProcedureThreshold::new(ProcedureName::UpdateSigners, 3),
    /// ];
    ///
    /// let account = client.create_account_with_proc_thresholds(
    ///     2,  // default 2-of-3
    ///     signer_commitments,
    ///     thresholds,
    /// ).await?;
    /// ```
    pub async fn create_account_with_proc_thresholds(
        &mut self,
        threshold: u32,
        signer_commitments: Vec<Word>,
        proc_threshold_overrides: Vec<ProcedureThreshold>,
    ) -> Result<&MultisigAccount> {
        let scheme = self.key_manager.scheme();
        let signers = signer_commitments
            .into_iter()
            .map(|commitment| SignerSpec::new(commitment, scheme))
            .collect();
        self.create_account_with_signers(threshold, signers, scheme, proc_threshold_overrides)
            .await
    }

    /// Creates a new multisig account whose approvers each carry their own
    /// signature scheme, for example an ECDSA wallet or hardware signer next to
    /// a locally held Falcon key.
    ///
    /// `guardian_scheme` selects the GUARDIAN key the account verifies
    /// acknowledgements with, independently of the approvers' schemes.
    /// [`Self::create_account_with_proc_thresholds`] is this with every
    /// approver and the GUARDIAN under the key manager's scheme. Registering an
    /// account whose approvers do not all use `guardian_scheme` needs a GUARDIAN
    /// with per-signer schemes (after 0.18.3).
    ///
    /// # Example
    ///
    /// ```ignore
    /// use miden_multisig_client::{SignatureScheme, SignerSpec};
    ///
    /// let account = client.create_account_with_signers(
    ///     2,
    ///     vec![
    ///         SignerSpec::new(client.user_commitment(), SignatureScheme::Falcon),
    ///         SignerSpec::new(wallet_commitment, SignatureScheme::Ecdsa),
    ///     ],
    ///     SignatureScheme::Falcon,
    ///     Vec::new(),
    /// ).await?;
    /// ```
    pub async fn create_account_with_signers(
        &mut self,
        threshold: u32,
        signers: Vec<SignerSpec>,
        guardian_scheme: SignatureScheme,
        proc_threshold_overrides: Vec<ProcedureThreshold>,
    ) -> Result<&MultisigAccount> {
        let signer_commitments = SignerSpec::commitments(&signers);
        Self::ensure_unique_signer_commitments(&signer_commitments)?;
        self.ensure_own_key_scheme(&signers)?;

        // Get GUARDIAN server's public key commitment
        let mut guardian_client = self.create_guardian_client().await?;
        let (guardian_commitment_hex, _raw_pubkey) = guardian_client
            .get_pubkey(Some(guardian_scheme.as_str()))
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!("failed to get GUARDIAN pubkey: {}", e))
            })?;

        let guardian_commitment =
            word_from_hex(&guardian_commitment_hex).map_err(MultisigError::HexDecode)?;

        // Convert procedure thresholds to (Word, u32) pairs
        let overrides: Vec<(Word, u32)> = proc_threshold_overrides
            .iter()
            .map(|pt| (pt.procedure_root(), pt.threshold))
            .collect();

        // Create the multisig account config
        let guardian_config =
            MultisigGuardianConfig::new(threshold, signer_commitments, guardian_commitment)
                .with_signer_schemes(signers.iter().map(|signer| signer.scheme).collect())
                .with_guardian_scheme(guardian_scheme)
                .with_proc_threshold_overrides(overrides);

        // Generate a random seed for account ID
        let mut seed = [0u8; 32];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut seed);

        let account = MultisigGuardianBuilder::new(guardian_config)
            .with_seed(seed)
            .build()
            .map_err(|e| MultisigError::MidenClient(format!("failed to build account: {}", e)))?;

        // Add to miden-client
        self.add_or_update_account(&account, false).await?;

        // Wrap in MultisigAccount and store
        let multisig_account = MultisigAccount::new(account);
        self.account = Some(multisig_account);

        Ok(self.account.as_ref().unwrap())
    }

    /// Pulls an account from GUARDIAN and loads it locally, replacing whatever
    /// the store held.
    ///
    /// Two uses: joining an existing multisig as a cosigner, and resetting a
    /// store known to be bad, which the demo does after a store error and a
    /// client reinitialization.
    ///
    /// The overwrite is unconditional on purpose, so do not give this the nonce
    /// reconciliation `sync_from_guardian` applies. That path protects a local
    /// account legitimately ahead of GUARDIAN between pushing a delta and its
    /// canonicalization. Here the caller has already decided local is worthless,
    /// and skipping the overwrite because a corrupt record carries a higher
    /// nonce would preserve the state this call exists to discard. Use
    /// `sync_from_guardian` to refresh an account you intend to keep.
    pub async fn pull_account(&mut self, account_id: AccountId) -> Result<&MultisigAccount> {
        let mut guardian_client = self.create_authenticated_guardian_client().await?;

        let state_response = guardian_client
            .get_state(&account_id)
            .await
            .map_err(|e| MultisigError::GuardianServer(format!("failed to get state: {}", e)))?;

        let state_obj = state_response.state.ok_or_else(|| {
            MultisigError::GuardianServer("no state returned from GUARDIAN".to_string())
        })?;

        let state_value: serde_json::Value = serde_json::from_str(&state_obj.state_json)?;

        let account_base64 = state_value["data"].as_str().ok_or_else(|| {
            MultisigError::GuardianServer("missing 'data' field in state".to_string())
        })?;

        let account_bytes = base64::engine::general_purpose::STANDARD
            .decode(account_base64)
            .map_err(|e| MultisigError::MidenClient(format!("failed to decode account: {}", e)))?;

        let account = Account::read_from_bytes(&account_bytes).map_err(|e| {
            MultisigError::MidenClient(format!("failed to deserialize account: {}", e))
        })?;

        self.add_or_update_account(&account, true).await?;

        let multisig_account = MultisigAccount::new(account);
        self.account = Some(multisig_account);

        Ok(self.account.as_ref().unwrap())
    }

    /// Pushes the current account to GUARDIAN for initial registration.
    ///
    /// The account is registered with the approver and GUARDIAN schemes it
    /// stores: under its single-scheme auth when every approver uses the
    /// GUARDIAN's scheme, and under the per-signer auth otherwise, which needs
    /// a GUARDIAN with per-signer schemes (after 0.18.3).
    ///
    /// GUARDIAN stores a state only when the chain holds it, unless it is a
    /// new account's nonce-0 state or the state GUARDIAN already stores. A
    /// state whose transaction has not landed yet, such as one right after a
    /// guardian switch, is refused as not on chain; register again once the
    /// transaction lands. A guardian switch waits for that itself before it
    /// registers on the new GUARDIAN.
    pub async fn push_account(&mut self) -> Result<()> {
        let account = self
            .account
            .as_ref()
            .ok_or_else(|| MultisigError::MissingConfig("no account loaded".to_string()))?;
        let auth_config =
            SignerSpec::guardian_auth_config(&account.cosigners()?, account.guardian_scheme()?);

        let mut guardian_client = self.create_authenticated_guardian_client().await?;

        let account_bytes = account.inner().to_bytes();
        let account_base64 = base64::engine::general_purpose::STANDARD.encode(&account_bytes);

        let initial_state = serde_json::json!({
            "data": account_base64,
            "account_id": account.id().to_string(),
        });

        let account_id = account.id();

        // Configure account on GUARDIAN
        guardian_client
            .configure(&account_id, auth_config, initial_state)
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!("failed to configure account: {}", e))
            })?;

        Ok(())
    }

    /// Polls the chain until it holds `commitment` for the account, about 45
    /// seconds in all. GUARDIAN registers a state past nonce 0 only once the
    /// chain holds it, and a read of the chain needs no wallet signature, so a
    /// guardian switch waits here and registers on the new GUARDIAN once.
    pub(crate) async fn wait_for_on_chain_state(
        &self,
        account_id: AccountId,
        commitment: Word,
    ) -> Result<()> {
        for poll in 0..LANDING_POLLS {
            if self.try_get_on_chain_account_commitment(account_id).await? == Some(commitment) {
                return Ok(());
            }
            if poll + 1 < LANDING_POLLS {
                tokio::time::sleep(retry_delay(poll, rand::random())).await;
            }
        }
        Err(MultisigError::GuardianServer(format!(
            "account {account_id} state {} has not reached the chain; register again once its transaction lands",
            word_to_hex(&commitment)
        )))
    }

    /// Syncs state with the Miden network.
    pub async fn sync(&mut self) -> Result<()> {
        self.sync_network_state().await?;

        let account_updated = self.sync_from_guardian_internal().await?;

        if account_updated {
            self.sync_network_state().await?;
        }

        self.refresh_cached_account_from_store().await
    }

    /// Syncs only with the Miden network and refreshes local cached account state.
    pub async fn sync_network_only(&mut self) -> Result<()> {
        self.sync_network_state().await?;
        self.refresh_cached_account_from_store().await
    }

    /// Syncs account state from GUARDIAN into the local miden-client store.
    pub async fn sync_from_guardian(&mut self) -> Result<()> {
        self.sync_from_guardian_internal().await?;
        Ok(())
    }

    async fn sync_network_state(&mut self) -> Result<()> {
        self.miden_client
            .sync_state()
            .await
            .map_err(|e| MultisigError::miden_client_with_context("failed to sync state", e))?;
        Ok(())
    }

    async fn refresh_cached_account_from_store(&mut self) -> Result<()> {
        if let Some(current) = self.account.take() {
            let account_id = current.id();
            let account_record = self
                .miden_client
                .get_account(account_id)
                .await
                .map_err(|e| {
                    MultisigError::miden_client_with_context("failed to get updated account", e)
                })?
                .ok_or_else(|| {
                    MultisigError::MissingConfig("account not found after sync".to_string())
                })?;
            let account: Account = account_record;
            let refreshed = MultisigAccount::new(account);
            self.account = Some(refreshed);
        }

        Ok(())
    }

    /// Explicitly verifies that local account state commitment matches on-chain commitment.
    pub async fn verify_state_commitment(&self) -> Result<StateVerificationResult> {
        let account = self.require_account()?;
        let account_id = account.id();
        let local_commitment = account.commitment();
        let on_chain_commitment = self.get_on_chain_account_commitment(account_id).await?;

        if local_commitment != on_chain_commitment {
            return Err(MultisigError::InvalidConfig(format!(
                "local account commitment does not match on-chain commitment for account {}: local={}, on_chain={}",
                account_id,
                word_to_hex(&local_commitment),
                word_to_hex(&on_chain_commitment)
            )));
        }

        Ok(StateVerificationResult {
            account_id,
            local_commitment_hex: word_to_hex(&local_commitment),
            on_chain_commitment_hex: word_to_hex(&on_chain_commitment),
        })
    }

    async fn ensure_safe_to_overwrite_local_state(
        &self,
        account_id: AccountId,
        incoming_commitment: Word,
    ) -> Result<()> {
        match self.try_get_on_chain_account_commitment(account_id).await? {
            None => Ok(()),
            Some(on_chain_commitment) if on_chain_commitment == incoming_commitment => Ok(()),
            Some(on_chain_commitment) => Err(MultisigError::InvalidConfig(format!(
                "refusing to overwrite local state: incoming commitment does not match on-chain commitment for account {}: incoming={}, on_chain={}",
                account_id,
                word_to_hex(&incoming_commitment),
                word_to_hex(&on_chain_commitment)
            ))),
        }
    }

    /// Whether the local state is a transaction that will not land, so that
    /// sync may roll it back to GUARDIAN's state even though GUARDIAN is not
    /// ahead. Execution writes membership, threshold and switch transactions
    /// to the store without a miden-client record
    /// (`rebuilds_local_state_from_delta`), so miden-client cannot undo them
    /// when the node drops them. The drop is judged as miden-client judges its
    /// own transactions
    /// ([`UntrackedSubmission::dropped_at`](super::UntrackedSubmission::dropped_at)):
    /// expiry is proof, the stale window a heuristic. GUARDIAN can forget a
    /// delta whose transaction is still pending (an expired retained row,
    /// retention disabled, a best-effort switch push that failed), so its
    /// absence alone proves nothing. All must hold: this client submitted the
    /// local state and that transaction is dropped by the store's sync
    /// height, miden-client has no pending record for the account, and the
    /// chain sits at GUARDIAN's canonical state. A transaction dropped only by
    /// the stale window also needs GUARDIAN not to hold the local state as a
    /// delta that may still land (`delta_may_still_land_as`); an expired one
    /// cannot land, so GUARDIAN is not asked. A failed read keeps local.
    async fn local_state_abandoned(
        &self,
        guardian_client: &mut GuardianClient,
        account_id: AccountId,
        local_nonce: u64,
        local_commitment: Word,
        guardian_commitment: Word,
    ) -> Result<bool> {
        let Some(submission) = self
            .untracked_submission
            .filter(|submission| submission.commitment == local_commitment)
        else {
            return Ok(false);
        };
        let sync_height = match self.miden_client.get_sync_height().await {
            Ok(sync_height) => sync_height,
            Err(error) => {
                tracing::warn!(%error, "failed to read the store's sync height; keeping the local state");
                return Ok(false);
            }
        };
        if !submission.dropped_at(sync_height) {
            return Ok(false);
        }

        let records = match self
            .miden_client
            .get_transactions(TransactionFilter::Uncommitted)
            .await
        {
            Ok(records) => records,
            Err(error) => {
                tracing::warn!(%error, "failed to read pending transactions; keeping the local state");
                return Ok(false);
            }
        };
        if records.into_iter().any(|record| {
            record.details.account_id == account_id
                && matches!(record.status, TransactionStatus::Pending)
        }) {
            return Ok(false);
        }

        match self.try_get_on_chain_account_commitment(account_id).await {
            Ok(Some(on_chain)) if on_chain == guardian_commitment => {}
            Ok(_) | Err(_) => return Ok(false),
        }
        if submission.expired_at(sync_height) {
            return Ok(true);
        }

        match guardian_client.get_delta(&account_id, local_nonce).await {
            Ok(response) => Ok(!response
                .delta
                .is_some_and(|delta| delta_may_still_land_as(&delta, local_commitment))),
            Err(e) if e.is_not_found() => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// Internal sync from GUARDIAN that returns whether the account was updated.
    ///
    /// Starts with the canonical-nonce pre-check (issue #191): when GUARDIAN's
    /// canonical head is not ahead of the local account (lower nonce, or the
    /// same nonce at the same commitment) there is nothing to pull, so the
    /// full state fetch is skipped. Otherwise — including the same nonce at a
    /// different commitment, which is divergence rather than staleness — the
    /// full sync below runs as it did before the pre-check.
    async fn sync_from_guardian_internal(&mut self) -> Result<bool> {
        let account = self.require_account()?;
        let account_id = account.id();
        let local_commitment = account.inner().to_commitment();
        let local_nonce = account.nonce();

        let mut guardian_client = self.create_authenticated_guardian_client().await?;
        let head = guardian_client
            .get_canonical_nonce(&account_id)
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!(
                    "failed to get canonical nonce from GUARDIAN: {}",
                    e
                ))
            })?;
        let head = CanonicalHead::from_response(&head)?;
        let mut abandoned_against = None;
        if head.is_not_ahead_of(local_nonce, local_commitment) {
            if head.commitment == local_commitment
                || !self
                    .local_state_abandoned(
                        &mut guardian_client,
                        account_id,
                        local_nonce,
                        local_commitment,
                        head.commitment,
                    )
                    .await?
            {
                return Ok(false);
            }
            abandoned_against = Some(head.commitment);
        }

        let state_response = guardian_client.get_state(&account_id).await.map_err(|e| {
            MultisigError::GuardianServer(format!("failed to get state from GUARDIAN: {}", e))
        })?;

        let state_obj = state_response.state.ok_or_else(|| {
            MultisigError::GuardianServer("no state returned from GUARDIAN".to_string())
        })?;

        // Parse GUARDIAN commitment
        let guardian_commitment_hex = &state_obj.commitment;
        let guardian_commitment =
            word_from_hex(guardian_commitment_hex).map_err(MultisigError::HexDecode)?;

        // Compare commitments - if they match, no update needed
        if local_commitment == guardian_commitment {
            return Ok(false);
        }

        // Commitments differ - deserialize GUARDIAN state to check nonce
        let state_value: serde_json::Value = serde_json::from_str(&state_obj.state_json)?;

        let account_base64 = state_value["data"].as_str().ok_or_else(|| {
            MultisigError::GuardianServer("missing 'data' field in state".to_string())
        })?;

        let account_bytes = base64::engine::general_purpose::STANDARD
            .decode(account_base64)
            .map_err(|e| MultisigError::MidenClient(format!("failed to decode account: {}", e)))?;

        let fresh_account = Account::read_from_bytes(&account_bytes).map_err(|e| {
            MultisigError::MidenClient(format!("failed to deserialize account: {}", e))
        })?;

        // Compare nonces - if local is newer or equal, don't overwrite with GUARDIAN's older state.
        // This happens after executing a transaction before GUARDIAN canonicalizes.
        let guardian_nonce = fresh_account.nonce().as_canonical_u64();
        let rolling_back = local_nonce >= guardian_nonce;
        if rolling_back
            && abandoned_against != Some(guardian_commitment)
            && !self
                .local_state_abandoned(
                    &mut guardian_client,
                    account_id,
                    local_nonce,
                    local_commitment,
                    guardian_commitment,
                )
                .await?
        {
            return Ok(false);
        }

        self.ensure_safe_to_overwrite_local_state(account_id, fresh_account.to_commitment())
            .await?;

        if rolling_back {
            let Some(notes) = self.input_notes_to_carry().await? else {
                tracing::warn!(
                    account_id = %account_id,
                    "Local state is a transaction that will not land, but an unconsumed input note \
                     could not be carried to a fresh store; keeping the local state"
                );
                return Ok(false);
            };
            tracing::warn!(
                account_id = %account_id,
                local_nonce,
                local_commitment = %word_to_hex(&local_commitment),
                guardian_nonce,
                guardian_commitment = %guardian_commitment_hex,
                replaced_store = %self.miden_store_path.display(),
                "Local state is a transaction that will not land; rolling back to GUARDIAN's state. \
                 The replaced store keeps the local state: should the transaction land after all, \
                 recover it from there"
            );
            self.roll_back_store(&fresh_account, &notes).await?;
        } else {
            // GUARDIAN has newer state - try to add/update.
            // If we get a commitment mismatch (locked state), reset and retry.
            match self.add_or_update_account(&fresh_account, true).await {
                Ok(()) => {}
                Err(e)
                    if e.to_string()
                        .contains("doesn't match the imported account commitment") =>
                {
                    // Reset miden-client and try again with fresh state
                    self.reset_miden_client().await?;
                    self.add_or_update_account(&fresh_account, true).await?;
                }
                Err(e) => return Err(e),
            }
        }

        let multisig_account = MultisigAccount::new(fresh_account);
        self.account = Some(multisig_account);

        Ok(true)
    }

    /// Replace the store with a fresh one holding `account` and `notes`, as a
    /// restarted client starts (miden-client and its store refuse to overwrite
    /// an account with a lower nonce). The fresh client is synced to the chain
    /// tip, which re-authenticates the carried notes, and installed only once
    /// all of that succeeded, so a failure keeps the current store, with every
    /// note it holds, for the next sync to retry.
    pub(crate) async fn roll_back_store(
        &mut self,
        account: &Account,
        notes: &[NoteFile],
    ) -> Result<()> {
        let (mut fresh_client, fresh_store, fresh_store_path) = self.fresh_miden_client().await?;
        fresh_client.add_account(account, true).await.map_err(|e| {
            MultisigError::miden_client_with_context(
                "failed to store the rolled-back account in a fresh store",
                e,
            )
        })?;
        if !notes.is_empty() {
            fresh_client.import_notes(notes).await.map_err(|e| {
                MultisigError::miden_client_with_context(
                    "failed to carry input notes over to the rolled-back store",
                    e,
                )
            })?;
        }
        fresh_client.sync_state().await.map_err(|e| {
            MultisigError::miden_client_with_context("failed to sync the rolled-back store", e)
        })?;
        self.miden_client = fresh_client;
        let replaced = std::mem::replace(&mut self.miden_store, fresh_store);
        self.replaced_stores.insert(0, replaced);
        self.miden_store_path = fresh_store_path;
        self.untracked_submission = None;
        Ok(())
    }

    /// The store's unconsumed input notes, as files a fresh store re-imports:
    /// each note's details and the tag sync finds it by (the inclusion proof is
    /// fetched again). `None` when a note's tag is unknown, so it could not be
    /// tracked again.
    async fn input_notes_to_carry(&self) -> Result<Option<Vec<NoteFile>>> {
        let records = self
            .miden_client
            .get_input_notes(NoteFilter::All)
            .await
            .map_err(|e| {
                MultisigError::miden_client_with_context("failed to read input notes", e)
            })?;
        let mut files = Vec::new();
        for record in records.iter().filter(|record| !record.is_consumed()) {
            let expected = match record.state() {
                InputNoteState::Expected(expected) => Some(expected),
                _ => None,
            };
            let Some(tag) = record
                .metadata()
                .map(|metadata| metadata.tag())
                .or_else(|| expected.and_then(|expected| expected.tag))
            else {
                return Ok(None);
            };
            let after_block_num = expected
                .map(|expected| expected.after_block_num)
                .unwrap_or_default();
            files.push(NoteFile::ExpectedNote {
                details: record.details().clone(),
                sync_hint: NoteSyncHint::new(after_block_num, tag),
            });
        }
        Ok(Some(files))
    }

    /// Fetches deltas from GUARDIAN since the current local nonce and applies them to the local account.
    pub async fn get_deltas(&mut self) -> Result<()> {
        let account = self.require_account()?.clone();
        let account_id = account.id();
        let current_nonce = account.nonce();
        let from_nonce = current_nonce.saturating_add(1);

        let mut guardian_client = self.create_authenticated_guardian_client().await?;
        let response = match guardian_client
            .get_delta_since(&account_id, from_nonce)
            .await
        {
            Ok(resp) => resp,
            // A not-found result means there are no new deltas since the current
            // nonce — a normal sync outcome, not a failure.
            Err(e) if e.is_not_found() => return Ok(()),
            Err(e) => {
                return Err(MultisigError::GuardianServer(format!(
                    "failed to pull deltas from GUARDIAN: {}",
                    e
                )));
            }
        };

        let merged_delta = response.merged_delta.ok_or_else(|| {
            MultisigError::GuardianServer("no merged_delta in response".to_string())
        })?;

        let expected_prev_commitment = if merged_delta.prev_commitment.is_empty() {
            None
        } else {
            Some(word_from_hex(&merged_delta.prev_commitment).map_err(MultisigError::HexDecode)?)
        };

        if let Some(prev_commitment) = expected_prev_commitment
            && account.commitment() != prev_commitment
        {
            return Ok(());
        }

        let tx_summary = merged_delta.try_into_tx_summary().map_err(|e| {
            MultisigError::MidenClient(format!("failed to parse delta payload: {}", e))
        })?;

        let account_delta = tx_summary.account_delta();

        let updated_account: Account = if guardian_shared::account_delta::is_account_creation(
            account.inner(),
            account_delta,
        ) {
            account_delta.try_to_new_account().map_err(|e| {
                MultisigError::MidenClient(format!(
                    "failed to convert full state delta to account: {}",
                    e
                ))
            })?
        } else {
            let mut acc: Account = account.into_inner();
            guardian_shared::account_delta::apply_account_delta(&mut acc, account_delta)
                .map_err(MultisigError::MidenClient)?;
            acc
        };

        self.ensure_safe_to_overwrite_local_state(account_id, updated_account.to_commitment())
            .await?;

        // Try to add/update account. If we get a commitment mismatch, reset the miden client
        // and re-import the account fresh from GUARDIAN to recover from locked/stale state.
        match self.add_or_update_account(&updated_account, true).await {
            Ok(()) => {
                let multisig_account = MultisigAccount::new(updated_account);
                self.account = Some(multisig_account);
                Ok(())
            }
            Err(e)
                if e.to_string()
                    .contains("doesn't match the imported account commitment") =>
            {
                // The miden-client store has the account in a stale/locked state.
                // Reset the client and re-pull fresh state from GUARDIAN.
                self.reset_miden_client().await?;

                // Re-pull fresh state from GUARDIAN
                let mut guardian_client = self.create_authenticated_guardian_client().await?;
                let state_response = guardian_client.get_state(&account_id).await.map_err(|e| {
                    MultisigError::GuardianServer(format!("failed to get state: {}", e))
                })?;

                let state_obj = state_response.state.ok_or_else(|| {
                    MultisigError::GuardianServer("no state returned from GUARDIAN".to_string())
                })?;

                let state_value: serde_json::Value = serde_json::from_str(&state_obj.state_json)?;

                let account_base64 = state_value["data"].as_str().ok_or_else(|| {
                    MultisigError::GuardianServer("missing 'data' field in state".to_string())
                })?;

                let account_bytes = base64::engine::general_purpose::STANDARD
                    .decode(account_base64)
                    .map_err(|e| {
                        MultisigError::MidenClient(format!("failed to decode account: {}", e))
                    })?;

                let fresh_account = Account::read_from_bytes(&account_bytes).map_err(|e| {
                    MultisigError::MidenClient(format!("failed to deserialize account: {}", e))
                })?;

                self.ensure_safe_to_overwrite_local_state(
                    account_id,
                    fresh_account.to_commitment(),
                )
                .await?;

                self.add_or_update_account(&fresh_account, true).await?;

                let multisig_account = MultisigAccount::new(fresh_account);
                self.account = Some(multisig_account);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Registers the current account on the GUARDIAN server.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // After switching GUARDIAN endpoints
    /// client.set_guardian_endpoint("http://new-guardian:50051");
    /// client.register_on_guardian().await?;
    /// ```
    pub async fn register_on_guardian(&mut self) -> Result<()> {
        self.push_account().await
    }

    /// Changes the GUARDIAN endpoint and optionally registers the account on the new server.
    ///
    /// When repointing to a different GUARDIAN provider after a switch, call
    /// [`MultisigClient::preserve_pre_switch_proposal_notes`] first — notes
    /// embedded in pending proposals are only importable while the old
    /// GUARDIAN is still the configured endpoint.
    ///
    /// # Arguments
    ///
    /// * `new_endpoint` - The new GUARDIAN server endpoint URL
    /// * `register` - If true, registers the current account on the new GUARDIAN server
    ///
    /// A Guardian session belongs to the old endpoint, so this drops it.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // GUARDIAN server moved to new URL (same keys, no on-chain change needed)
    /// client.set_guardian_endpoint("http://new-guardian:50051", true).await?;
    /// ```
    pub async fn set_guardian_endpoint(
        &mut self,
        new_endpoint: &str,
        register: bool,
    ) -> Result<()> {
        self.guardian_endpoint = new_endpoint.to_string();
        self.guardian_session = guardian_client::SessionSlot::default();

        if register {
            self.register_on_guardian().await?;
        }

        Ok(())
    }
}

/// Chain reads while waiting for a state to land. With [`retry_delay`]'s
/// backoff the last one runs about 45 seconds after the first, past the time
/// a transaction takes to land.
const LANDING_POLLS: u32 = 10;

/// The nonce and commitment GUARDIAN reports for its canonical state
/// (`get_canonical_nonce`), parsed at the transport boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CanonicalHead {
    nonce: u64,
    commitment: Word,
}

/// Whether GUARDIAN holds `local_commitment` as a delta that may still land:
/// one recording that state in any status but discarded. A commitment that
/// does not parse may be it.
fn delta_may_still_land_as(delta: &guardian_client::DeltaObject, local_commitment: Word) -> bool {
    use guardian_client::delta_status::Status;
    if matches!(
        delta
            .status
            .as_ref()
            .and_then(|status| status.status.as_ref()),
        Some(Status::DiscardedAt(_))
    ) {
        return false;
    }
    word_from_hex(&delta.new_commitment).map_or(true, |recorded| recorded == local_commitment)
}

impl CanonicalHead {
    pub(crate) fn from_response(
        response: &guardian_client::GetCanonicalNonceResponse,
    ) -> Result<Self> {
        Ok(Self {
            nonce: response.nonce,
            commitment: word_from_hex(&response.commitment).map_err(MultisigError::HexDecode)?,
        })
    }

    /// GUARDIAN has nothing newer than the local account when its head is
    /// below the local nonce, or at the local nonce with the same commitment.
    /// The same nonce at a different commitment is divergence, which the
    /// full state fetch is left to reconcile.
    pub(crate) fn is_not_ahead_of(&self, local_nonce: u64, local_commitment: Word) -> bool {
        match self.nonce.cmp(&local_nonce) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => self.commitment == local_commitment,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(value: u32) -> Word {
        Word::from([value, 0, 0, 0])
    }

    /// A GUARDIAN head at the given account nonce (a transaction counter,
    /// not a cryptographic nonce) and commitment.
    fn head(account_nonce: u64, commitment: u32) -> CanonicalHead {
        CanonicalHead {
            nonce: account_nonce,
            commitment: word(commitment),
        }
    }

    const LOCAL_ACCOUNT_NONCE: u64 = 2;

    #[test]
    fn canonical_head_is_not_ahead_below_or_at_the_local_head() {
        let local = LOCAL_ACCOUNT_NONCE;
        assert!(head(local - 1, 7).is_not_ahead_of(local, word(7)));
        assert!(head(local - 1, 9).is_not_ahead_of(local, word(7)));
        assert!(head(local, 7).is_not_ahead_of(local, word(7)));
    }

    #[test]
    fn canonical_head_is_ahead_above_the_local_nonce_or_diverged_at_it() {
        let local = LOCAL_ACCOUNT_NONCE;
        assert!(!head(local + 1, 7).is_not_ahead_of(local, word(7)));
        assert!(!head(local, 9).is_not_ahead_of(local, word(7)));
    }

    #[test]
    fn canonical_head_rejects_a_malformed_commitment() {
        let response = guardian_client::GetCanonicalNonceResponse {
            success: true,
            message: String::new(),
            account_id: String::new(),
            nonce: 1,
            commitment: "not-hex".to_string(),
            error_code: String::new(),
        };
        assert!(matches!(
            CanonicalHead::from_response(&response),
            Err(MultisigError::HexDecode(_))
        ));
    }

    #[test]
    fn ensure_unique_signer_commitments_rejects_duplicates() {
        let result = MultisigClient::ensure_unique_signer_commitments(&[word(1), word(2), word(1)]);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("duplicate signer commitment")
        );
    }
}
