//! Account lifecycle operations for MultisigClient.
//!
//! This module handles account creation, pulling/pushing from PSM,
//! syncing, and registration operations.

use base64::Engine;
use miden_client::account::Account;
use miden_client::{Deserializable, Serializable};
use miden_confidential_contracts::multisig_psm::{MultisigPsmBuilder, MultisigPsmConfig};
use miden_objects::Word;
use miden_objects::account::AccountId;
use private_state_manager_client::{
    AuthConfig, ClientError as PsmClientError, MidenFalconRpoAuth, TryIntoTxSummary,
    auth_config::AuthType,
};

use super::MultisigClient;
use crate::account::MultisigAccount;
use crate::config::ProcedureThreshold;
use crate::error::{MultisigError, Result};

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
    pub async fn sync(&mut self) -> Result<()> {
        self.get_deltas().await?;

        self.miden_client
            .sync_state()
            .await
            .map_err(|e| MultisigError::MidenClient(format!("failed to sync state: {:#?}", e)))?;

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
            let refreshed = MultisigAccount::new(account_record.into(), &self.psm_endpoint);
            self.account = Some(refreshed);
        }

        Ok(())
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

        self.add_or_update_account(&updated_account, true).await?;

        let multisig_account = MultisigAccount::new(updated_account, &self.psm_endpoint);
        self.account = Some(multisig_account);

        Ok(())
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
