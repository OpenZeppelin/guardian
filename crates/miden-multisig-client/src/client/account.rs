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
use private_state_manager_client::{AuthConfig, MidenFalconRpoAuth, auth_config::AuthType};

use super::MultisigClient;
use crate::account::MultisigAccount;
use crate::error::{MultisigError, Result};
use crate::sync::sync_miden_state;

impl MultisigClient {
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

    /// Syncs state with the Miden network.
    ///
    /// If sync panics due to corrupted local state (a known issue in miden-client v0.12.x
    /// with partial MMR data), this method will automatically reset the local database
    /// and retry once. Regular sync errors (network, timeout, etc.) are propagated without
    /// resetting.
    pub async fn sync(&mut self) -> Result<()> {
        match sync_miden_state(&mut self.miden_client).await {
            Ok(()) => Ok(()),
            Err(MultisigError::SyncPanicked(msg)) => {
                // WORKAROUND: miden-client v0.12.x can panic on sync when the local
                // MMR state becomes inconsistent (e.g., "if there is an odd element, a merge
                // is required" panic in partial_mmr.rs). This can happen after certain
                // transaction patterns. We recover by clearing the local state and re-syncing.
                //
                // This is safe because:
                // 1. On-chain state is the source of truth
                // 2. Account data can be re-fetched from the network
                // 3. The account itself is not stored in the SQLite DB (we keep it in memory)
                //
                // Only panics trigger recovery - regular errors (network, timeout) are
                // propagated so the caller can handle them appropriately.
                //
                // TODO: Remove this workaround when miden-client is updated with a fix.
                eprintln!(
                    "Sync panicked ({}), attempting recovery by resetting local state...",
                    msg
                );
                self.reset_miden_client().await?;
                sync_miden_state(&mut self.miden_client).await
            }
            Err(e) => Err(e), // Propagate regular errors without reset
        }
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
}
