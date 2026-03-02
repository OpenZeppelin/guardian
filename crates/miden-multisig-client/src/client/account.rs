//! Account lifecycle operations for MultisigClient.
//!
//! This module handles account creation, pulling/pushing from PSM,
//! syncing, and registration operations.

use base64::Engine;
use miden_client::account::Account;
use miden_client::{Deserializable, Serializable};
use miden_confidential_contracts::multisig_psm::{MultisigPsmBuilder, MultisigPsmConfig};
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use private_state_manager_client::{
    AuthConfig, ClientError as PsmClientError, MidenFalconRpoAuth, TryIntoTxSummary,
    auth_config::AuthType,
};

use super::{MultisigClient, StateVerificationResult};
use crate::account::MultisigAccount;
use crate::config::ProcedureThreshold;
use crate::error::{MultisigError, Result};
use crate::transaction::word_to_hex;

impl MultisigClient {
    /// Creates a new multisig account.
    ///
    /// # Arguments
    /// * `threshold` - Minimum number of signatures required (default threshold)
    /// * `signer_commitments` - Public key commitments of all signers
    ///
    /// For per-procedure thresholds, use `create_account_with_config` instead.
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
        // Get PSM server's public key commitment
        let mut psm_client = self.create_psm_client().await?;
        let psm_pubkey_hex = psm_client
            .get_pubkey()
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to get PSM pubkey: {}", e)))?;

        let psm_commitment = crate::keystore::commitment_from_hex(&psm_pubkey_hex)
            .map_err(MultisigError::HexDecode)?;

        // Convert procedure thresholds to (Word, u32) pairs
        let overrides: Vec<(Word, u32)> = proc_threshold_overrides
            .iter()
            .map(|pt| (pt.procedure_root(), pt.threshold))
            .collect();

        // Create the multisig account config
        let psm_config = MultisigPsmConfig::new(threshold, signer_commitments, psm_commitment)
            .with_proc_threshold_overrides(overrides);

        // Generate a random seed for account ID
        let mut seed = [0u8; 32];
        rand::Rng::fill(&mut rand::rng(), &mut seed);

        let account = MultisigPsmBuilder::new(psm_config)
            .with_seed(seed)
            .build()
            .map_err(|e| MultisigError::MidenClient(format!("failed to build account: {}", e)))?;

        // Add to miden-client
        self.add_or_update_account(&account, false).await?;

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

        let state_obj = state_response
            .state
            .ok_or_else(|| MultisigError::PsmServer("no state returned from PSM".to_string()))?;

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

        self.add_or_update_account(&account, true).await?;

        let multisig_account = MultisigAccount::new(account, &self.psm_endpoint);
        self.account = Some(multisig_account);

        Ok(self.account.as_ref().unwrap())
    }

    /// Pushes the current account to PSM for initial registration.
    pub async fn push_account(&mut self) -> Result<()> {
        let account = self
            .account
            .as_ref()
            .ok_or_else(|| MultisigError::MissingConfig("no account loaded".to_string()))?;

        let mut psm_client = self.create_authenticated_psm_client().await?;

        let account_bytes = account.inner().to_bytes();
        let account_base64 = base64::engine::general_purpose::STANDARD.encode(&account_bytes);

        let initial_state = serde_json::json!({
            "data": account_base64,
            "account_id": account.id().to_string(),
        });

        let cosigner_commitments = account.cosigner_commitments_hex();
        let auth_config = AuthConfig {
            auth_type: Some(AuthType::MidenFalconRpo(MidenFalconRpoAuth {
                cosigner_commitments,
            })),
        };

        let account_id = account.id();

        // Configure account on PSM
        psm_client
            .configure(&account_id, auth_config, initial_state)
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to configure account: {}", e)))?;

        Ok(())
    }

    /// Syncs state with the Miden network.
    ///
    /// This follows the same approach as the web client's syncState():
    /// 1. Sync with Miden network first to ensure we have latest block headers
    /// 2. Fetch state from PSM
    /// 3. Compare PSM commitment with local commitment
    /// 4. If they differ, overwrite local with PSM state
    /// 5. If account was updated from PSM, sync with network again
    pub async fn sync(&mut self) -> Result<()> {
        // First, sync with Miden network to get latest block headers.
        // This is critical - without block headers, transaction execution will fail.
        self.miden_client
            .sync_state()
            .await
            .map_err(|e| MultisigError::MidenClient(format!("failed to sync state: {:#?}", e)))?;

        // Then sync state from PSM (like web client's syncState)
        let account_updated = self.sync_from_psm_internal().await?;

        // If PSM updated our account, sync with network again to ensure
        // block headers are consistent with the new account state.
        if account_updated {
            self.miden_client.sync_state().await.map_err(|e| {
                MultisigError::MidenClient(format!("failed to sync after PSM update: {:#?}", e))
            })?;
        }

        // Refresh cached account (commitment/nonce/etc.) from the miden-client store
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
            let refreshed = MultisigAccount::new(account, &self.psm_endpoint);
            self.account = Some(refreshed);
        }

        Ok(())
    }

    /// Syncs account state from PSM into the local miden-client store.
    ///
    /// This mirrors the web client's syncState() approach:
    /// - Fetches full state from PSM
    /// - Compares PSM commitment with local commitment
    /// - If they differ and PSM has newer state, overwrites local with PSM state
    /// - If local is newer (e.g., after execution before PSM canonicalizes), keeps local
    ///
    /// This is simpler and more robust than applying incremental deltas.
    pub async fn sync_from_psm(&mut self) -> Result<()> {
        self.sync_from_psm_internal().await?;
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

    /// Internal sync from PSM that returns whether the account was updated.
    async fn sync_from_psm_internal(&mut self) -> Result<bool> {
        let account = self.require_account()?;
        let account_id = account.id();
        let local_commitment = account.inner().commitment();
        let local_nonce = account.nonce();

        // Fetch state from PSM
        let mut psm_client = self.create_authenticated_psm_client().await?;
        let state_response = psm_client.get_state(&account_id).await.map_err(|e| {
            MultisigError::PsmServer(format!("failed to get state from PSM: {}", e))
        })?;

        let state_obj = state_response
            .state
            .ok_or_else(|| MultisigError::PsmServer("no state returned from PSM".to_string()))?;

        // Parse PSM commitment
        let psm_commitment_hex = &state_obj.commitment;
        let psm_commitment =
            crate::commitment_from_hex(psm_commitment_hex).map_err(MultisigError::HexDecode)?;

        // Compare commitments - if they match, no update needed
        if local_commitment == psm_commitment {
            return Ok(false);
        }

        // Commitments differ - deserialize PSM state to check nonce
        let state_value: serde_json::Value = serde_json::from_str(&state_obj.state_json)?;

        let account_base64 = state_value["data"]
            .as_str()
            .ok_or_else(|| MultisigError::PsmServer("missing 'data' field in state".to_string()))?;

        let account_bytes = base64::engine::general_purpose::STANDARD
            .decode(account_base64)
            .map_err(|e| MultisigError::MidenClient(format!("failed to decode account: {}", e)))?;

        let fresh_account = Account::read_from_bytes(&account_bytes).map_err(|e| {
            MultisigError::MidenClient(format!("failed to deserialize account: {}", e))
        })?;

        // Compare nonces - if local is newer or equal, don't overwrite with PSM's older state.
        // This happens after executing a transaction before PSM canonicalizes.
        let psm_nonce = fresh_account.nonce().as_int();
        if local_nonce >= psm_nonce {
            // Local state is newer, skip PSM update
            return Ok(false);
        }

        // PSM has newer state - try to add/update.
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

        let multisig_account = MultisigAccount::new(fresh_account, &self.psm_endpoint);
        self.account = Some(multisig_account);

        Ok(true)
    }

    /// Fetches deltas from PSM since the current local nonce and applies them to the local account.
    pub async fn get_deltas(&mut self) -> Result<()> {
        let account = self.require_account()?.clone();
        let account_id = account.id();
        let current_nonce = account.nonce();

        let mut psm_client = self.create_authenticated_psm_client().await?;
        let response = match psm_client.get_delta_since(&account_id, current_nonce).await {
            Ok(resp) => resp,
            Err(PsmClientError::ServerError(msg)) if msg.contains("not found") => {
                // No new deltas since current nonce - this is not an error
                return Ok(());
            }
            Err(e) => {
                return Err(MultisigError::PsmServer(format!(
                    "failed to pull deltas from PSM: {}",
                    e
                )));
            }
        };

        let merged_delta = response
            .merged_delta
            .ok_or_else(|| MultisigError::PsmServer("no merged_delta in response".to_string()))?;

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

        // Try to add/update account. If we get a commitment mismatch, reset the miden client
        // and re-import the account fresh from PSM to recover from locked/stale state.
        match self.add_or_update_account(&updated_account, true).await {
            Ok(()) => {
                let multisig_account = MultisigAccount::new(updated_account, &self.psm_endpoint);
                self.account = Some(multisig_account);
                Ok(())
            }
            Err(e)
                if e.to_string()
                    .contains("doesn't match the imported account commitment") =>
            {
                // The miden-client store has the account in a stale/locked state.
                // Reset the client and re-pull fresh state from PSM.
                self.reset_miden_client().await?;

                // Re-pull fresh state from PSM
                let mut psm_client = self.create_authenticated_psm_client().await?;
                let state_response = psm_client
                    .get_state(&account_id)
                    .await
                    .map_err(|e| MultisigError::PsmServer(format!("failed to get state: {}", e)))?;

                let state_obj = state_response.state.ok_or_else(|| {
                    MultisigError::PsmServer("no state returned from PSM".to_string())
                })?;

                let state_value: serde_json::Value = serde_json::from_str(&state_obj.state_json)?;

                let account_base64 = state_value["data"].as_str().ok_or_else(|| {
                    MultisigError::PsmServer("missing 'data' field in state".to_string())
                })?;

                let account_bytes = base64::engine::general_purpose::STANDARD
                    .decode(account_base64)
                    .map_err(|e| {
                        MultisigError::MidenClient(format!("failed to decode account: {}", e))
                    })?;

                let fresh_account = Account::read_from_bytes(&account_bytes).map_err(|e| {
                    MultisigError::MidenClient(format!("failed to deserialize account: {}", e))
                })?;

                self.add_or_update_account(&fresh_account, true).await?;

                let multisig_account = MultisigAccount::new(fresh_account, &self.psm_endpoint);
                self.account = Some(multisig_account);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Syncs account state from PSM and updates the local cache.
    pub async fn sync_account(&mut self) -> Result<()> {
        if self.account().is_some() {
            self.sync().await
        } else {
            let account_id = self.require_account()?.id();
            self.pull_account(account_id).await?;
            Ok(())
        }
    }

    /// Registers the current account on the PSM server.
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
}
