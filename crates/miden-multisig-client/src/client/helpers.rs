//! Internal helper functions for PSM client interactions.

use miden_client::account::Account;
use miden_objects::account::AccountId;
use miden_objects::account::auth::Signature as AccountSignature;
use miden_objects::crypto::dsa::rpo_falcon512::Signature as RpoFalconSignature;
use private_state_manager_client::{Auth, FalconRpoSigner, PsmClient};
use private_state_manager_shared::hex::FromHex;

use super::MultisigClient;
use crate::account::MultisigAccount;
use crate::builder::create_miden_client;
use crate::error::{MultisigError, Result};
use crate::proposal::TransactionType;

impl MultisigClient {
    /// Creates a PSM client (unauthenticated).
    pub(crate) async fn create_psm_client(&self) -> Result<PsmClient> {
        PsmClient::connect(&self.psm_endpoint)
            .await
            .map_err(|e| MultisigError::PsmConnection(e.to_string()))
    }

    /// Creates an authenticated PSM client.
    pub(crate) async fn create_authenticated_psm_client(&self) -> Result<PsmClient> {
        let client = self.create_psm_client().await?;

        // Create Auth from our key manager's secret key
        let secret_key = self.key_manager.clone_secret_key();
        let signer = FalconRpoSigner::new(secret_key);
        let auth = Auth::FalconRpoSigner(signer);

        Ok(client.with_auth(auth))
    }

    /// Returns a reference to the current account, or error if none loaded.
    pub(crate) fn require_account(&self) -> Result<&MultisigAccount> {
        self.account
            .as_ref()
            .ok_or_else(|| MultisigError::MissingConfig("no account loaded".to_string()))
    }

    /// Gets the PSM acknowledgment signature for a transaction.
    ///
    /// This pushes the delta to PSM and retrieves the server's signature.
    pub(crate) async fn get_psm_ack_signature(
        &mut self,
        account: &MultisigAccount,
        nonce: u64,
        tx_summary: &miden_client::transaction::TransactionSummary,
        tx_summary_commitment: miden_objects::Word,
    ) -> Result<crate::execution::SignatureAdvice> {
        use private_state_manager_shared::ToJson;

        let account_id = account.id();
        let prev_commitment = format!("0x{}", hex::encode(account.commitment().as_bytes()));

        // Push delta to PSM to get acknowledgment signature
        let mut psm_client = self.create_authenticated_psm_client().await?;
        let delta_payload = tx_summary.to_json();

        let push_response = psm_client
            .push_delta(&account_id, nonce, &prev_commitment, &delta_payload)
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

        // Parse and build advice entry
        let ack_sig_with_prefix = crate::keystore::ensure_hex_prefix(&ack_sig);
        let ack_signature = RpoFalconSignature::from_hex(&ack_sig_with_prefix).map_err(|e| {
            MultisigError::Signature(format!("failed to parse PSM ack signature: {}", e))
        })?;

        let psm_commitment = crate::keystore::commitment_from_hex(&psm_commitment_hex)
            .map_err(MultisigError::HexDecode)?;

        Ok(crate::transaction::build_signature_advice_entry(
            psm_commitment,
            tx_summary_commitment,
            &AccountSignature::from(ack_signature),
        ))
    }

    /// Finalizes a transaction by executing it on-chain and updating local state.
    ///
    /// This handles the common post-execution logic for all proposal types.
    pub(crate) async fn finalize_transaction(
        &mut self,
        account_id: AccountId,
        tx_request: miden_client::transaction::TransactionRequest,
        transaction_type: &TransactionType,
    ) -> Result<()> {
        // Capture the new PSM endpoint if this is a SwitchPsm transaction
        let new_psm_endpoint =
            if let TransactionType::SwitchPsm { new_endpoint, .. } = transaction_type {
                Some(new_endpoint.clone())
            } else {
                None
            };

        // Execute the transaction on-chain
        self.miden_client
            .submit_new_transaction(account_id, tx_request)
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

    /// Resets the miden-client by clearing the SQLite database and recreating the client.
    ///
    /// WORKAROUND: This is a recovery mechanism for miden-client v0.12.x issues where
    /// the local partial MMR state can become corrupted, causing sync to panic.
    ///
    /// This preserves:
    /// - The in-memory account state (re-added to the new client)
    /// - PSM connection and credentials
    /// - All key material
    ///
    /// After reset, sync will fetch notes from the network again.
    pub(crate) async fn reset_miden_client(&mut self) -> Result<()> {
        let store_path = self.account_dir.join("miden-client.sqlite");
        let backup_path = self.account_dir.join("miden-client.sqlite.corrupt");

        // Rename the corrupt DB file to free up the original path.
        // This works even with open file handles on Unix (the old client still
        // holds the renamed file). On Windows rename may fail, but we try anyway.
        if store_path.exists() {
            let _ = std::fs::rename(&store_path, &backup_path);
        }

        // Create new client with fresh DB at original path
        self.miden_client = create_miden_client(&self.account_dir, &self.miden_endpoint).await?;

        // Clean up old files (best effort - may fail on Windows with open handles)
        let _ = std::fs::remove_file(&backup_path);
        let _ = std::fs::remove_file(self.account_dir.join("miden-client.sqlite-wal"));
        let _ = std::fs::remove_file(self.account_dir.join("miden-client.sqlite-shm"));

        // Re-add the account to the new miden-client so sync can discover notes for it
        if let Some(account) = &self.account {
            self.miden_client
                .add_account(account.inner(), true) // true = imported
                .await
                .map_err(|e| {
                    MultisigError::MidenClient(format!(
                        "failed to re-add account after reset: {}",
                        e
                    ))
                })?;
        }

        eprintln!("Local state reset successfully. Re-syncing...");
        Ok(())
    }
}
