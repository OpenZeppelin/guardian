//! Account lifecycle operations for MultisigClient.
//!
//! This module handles account creation, pulling/pushing from GUARDIAN,
//! syncing, and registration operations.

use std::collections::HashSet;

use base64::Engine;
use guardian_client::{
    AuthConfig, ClientError as GuardianClientError, MidenEcdsaAuth, MidenFalconRpoAuth,
    TryIntoTxSummary, auth_config::AuthType,
};
use guardian_shared::SignatureScheme;
use miden_client::account::Account;
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

    /// Creates a new multisig account.
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
        Self::ensure_unique_signer_commitments(&signer_commitments)?;
        let signature_scheme = self.key_manager.scheme();

        // Get GUARDIAN server's public key commitment
        let mut guardian_client = self.create_guardian_client().await?;
        let (guardian_commitment_hex, _raw_pubkey) = guardian_client
            .get_pubkey(Some(&signature_scheme.to_string()))
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
                .with_signature_scheme(signature_scheme)
                .with_proc_threshold_overrides(overrides);

        // Generate a random seed for account ID
        let mut seed = [0u8; 32];
        rand::Rng::fill(&mut rand::rng(), &mut seed);

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

    /// Pulls an account from GUARDIAN and loads it locally.
    ///
    /// Use this when joining an existing multisig as a cosigner.
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
    pub async fn push_account(&mut self) -> Result<()> {
        let account = self
            .account
            .as_ref()
            .ok_or_else(|| MultisigError::MissingConfig("no account loaded".to_string()))?;

        let mut guardian_client = self.create_authenticated_guardian_client().await?;

        let account_bytes = account.inner().to_bytes();
        let account_base64 = base64::engine::general_purpose::STANDARD.encode(&account_bytes);

        let initial_state = serde_json::json!({
            "data": account_base64,
            "account_id": account.id().to_string(),
        });

        let cosigner_commitments = account.cosigner_commitments_hex();
        let auth_config = AuthConfig {
            auth_type: Some(match self.key_manager.scheme() {
                SignatureScheme::Falcon => AuthType::MidenFalconRpo(MidenFalconRpoAuth {
                    cosigner_commitments,
                }),
                SignatureScheme::Ecdsa => AuthType::MidenEcdsa(MidenEcdsaAuth {
                    cosigner_commitments,
                }),
            }),
        };

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
            .map_err(|e| MultisigError::MidenClient(format!("failed to sync state: {:#?}", e)))?;
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
                    MultisigError::MidenClient(format!("failed to get updated account: {}", e))
                })?
                .ok_or_else(|| {
                    MultisigError::MissingConfig("account not found after sync".to_string())
                })?;
            let account: Account = account_record.try_into().map_err(|e| {
                MultisigError::MidenClient(format!("account record is not full: {}", e))
            })?;
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
    /// Internal sync from GUARDIAN that returns whether the account was updated.
    async fn sync_from_guardian_internal(&mut self) -> Result<bool> {
        let account = self.require_account()?;
        let account_id = account.id();
        let local_commitment = account.inner().to_commitment();
        let local_nonce = account.nonce();

        // Fetch state from GUARDIAN
        let mut guardian_client = self.create_authenticated_guardian_client().await?;
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
        if local_nonce >= guardian_nonce {
            // Local state is newer, skip GUARDIAN update
            return Ok(false);
        }

        self.ensure_safe_to_overwrite_local_state(account_id, fresh_account.to_commitment())
            .await?;

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

        let multisig_account = MultisigAccount::new(fresh_account);
        self.account = Some(multisig_account);

        Ok(true)
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
            Err(GuardianClientError::ServerError(msg)) if msg.contains("not found") => {
                // No new deltas since current nonce - this is not an error
                return Ok(());
            }
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

        let updated_account: Account = if account_delta.is_full_state() {
            Account::try_from(account_delta).map_err(|e| {
                MultisigError::MidenClient(format!(
                    "failed to convert full state delta to account: {}",
                    e
                ))
            })?
        } else {
            let mut acc: Account = account.into_inner();
            acc.apply_delta(account_delta).map_err(|e| {
                MultisigError::MidenClient(format!("failed to apply delta to account: {}", e))
            })?;
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
    /// # Arguments
    ///
    /// * `new_endpoint` - The new GUARDIAN server endpoint URL
    /// * `register` - If true, registers the current account on the new GUARDIAN server
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

        if register {
            self.register_on_guardian().await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(value: u32) -> Word {
        Word::from([value, 0, 0, 0])
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
