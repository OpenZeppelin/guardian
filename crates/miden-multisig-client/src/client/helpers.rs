//! Internal helper functions for PSM client interactions.

use miden_client::account::Account;
use miden_objects::account::AccountId;
use miden_objects::account::auth::Signature as AccountSignature;
use miden_objects::crypto::dsa::rpo_falcon512::Signature as RpoFalconSignature;
use miden_objects::utils::Deserializable;
use private_state_manager_client::{Auth, EcdsaSigner, FalconRpoSigner, PsmClient};
use private_state_manager_shared::ToJson;
use private_state_manager_shared::hex::FromHex;

use super::MultisigClient;
use crate::account::MultisigAccount;
use crate::builder::create_miden_client;
use crate::error::{MultisigError, Result};
use crate::execution::SignatureAdvice;
use crate::keystore::{SchemeSecretKey, commitment_from_hex, ensure_hex_prefix};
use crate::proposal::TransactionType;
use crate::transaction::{build_ecdsa_signature_advice_entry, build_signature_advice_entry};

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

        let auth = match self.key_manager.secret_key() {
            SchemeSecretKey::Falcon(sk) => Auth::FalconRpoSigner(FalconRpoSigner::new(sk)),
            SchemeSecretKey::Ecdsa(sk) => Auth::EcdsaSigner(EcdsaSigner::new(sk)),
        };

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
    ) -> Result<SignatureAdvice> {
        let account_id = account.id();
        let prev_commitment = format!(
            "0x{}",
            hex::encode(miden_objects::utils::serde::Serializable::to_bytes(
                &account.commitment(),
            ))
        );

        let mut psm_client = self.create_authenticated_psm_client().await?;
        let delta_payload = tx_summary.to_json();

        let push_response = psm_client
            .push_delta(&account_id, nonce, &prev_commitment, &delta_payload)
            .await
            .map_err(|e| MultisigError::PsmServer(format!("failed to push delta: {}", e)))?;

        let ack_sig = push_response.ack_sig.ok_or_else(|| {
            MultisigError::PsmServer("PSM did not return acknowledgment signature".to_string())
        })?;

        let delta = push_response.delta;
        let ack_scheme = delta
            .as_ref()
            .and_then(|d| d.ack_scheme.as_deref())
            .unwrap_or("falcon");
        let ack_pubkey = delta.as_ref().and_then(|d| d.ack_pubkey.clone());

        let psm_commitment_hex = psm_client.get_pubkey().await.map_err(|e| {
            MultisigError::PsmServer(format!("failed to get PSM commitment: {}", e))
        })?;

        let psm_commitment =
            commitment_from_hex(&psm_commitment_hex).map_err(MultisigError::HexDecode)?;

        parse_ack_signature(
            &ack_sig,
            ack_scheme,
            ack_pubkey,
            psm_commitment,
            tx_summary_commitment,
        )
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
        let new_psm_endpoint =
            if let TransactionType::SwitchPsm { new_endpoint, .. } = transaction_type {
                Some(new_endpoint.clone())
            } else {
                None
            };

        self.miden_client
            .submit_new_transaction(account_id, tx_request)
            .await
            .map_err(|e| {
                MultisigError::TransactionExecution(format!(
                    "transaction execution failed: {:?}",
                    e
                ))
            })?;

        self.sync().await?;

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

        if let Some(endpoint) = new_psm_endpoint {
            self.psm_endpoint = endpoint;

            let multisig_account =
                MultisigAccount::new(updated_account.clone(), &self.psm_endpoint);
            self.account = Some(multisig_account);

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

    /// Resets the miden-client by creating a new instance with a fresh database.
    pub async fn reset_miden_client(&mut self) -> Result<()> {
        self.miden_client = create_miden_client(&self.account_dir, &self.miden_endpoint).await?;
        Ok(())
    }

    /// Adds an account to miden-client if it doesn't exist, or updates it if it does.
    pub(crate) async fn add_or_update_account(
        &mut self,
        account: &Account,
        imported: bool,
    ) -> Result<()> {
        let account_id = account.id();

        let existing = self
            .miden_client
            .get_account(account_id)
            .await
            .map_err(|e| MultisigError::MidenClient(format!("failed to check account: {}", e)))?;

        if existing.is_some() {
            self.miden_client
                .add_account(account, true)
                .await
                .map_err(|e| {
                    MultisigError::MidenClient(format!("failed to update account: {}", e))
                })?;
        } else {
            self.miden_client
                .add_account(account, imported)
                .await
                .map_err(|e| MultisigError::MidenClient(format!("failed to add account: {}", e)))?;
        }

        Ok(())
    }
}

/// Parses an ack signature from PSM into a `SignatureAdvice` entry.
fn parse_ack_signature(
    ack_sig_hex: &str,
    ack_scheme: &str,
    ack_pubkey: Option<String>,
    psm_commitment: miden_objects::Word,
    tx_summary_commitment: miden_objects::Word,
) -> Result<SignatureAdvice> {
    let ack_sig_with_prefix = ensure_hex_prefix(ack_sig_hex);
    if ack_scheme.eq_ignore_ascii_case("ecdsa") {
        let hex_str = ack_sig_with_prefix.trim_start_matches("0x");
        let sig_bytes = hex::decode(hex_str).map_err(|e| {
            MultisigError::Signature(format!("invalid ECDSA ack signature hex: {}", e))
        })?;
        let ecdsa_sig =
            miden_objects::crypto::dsa::ecdsa_k256_keccak::Signature::read_from_bytes(&sig_bytes)
                .map_err(|e| {
                MultisigError::Signature(format!(
                    "failed to deserialize ECDSA ack signature: {}",
                    e
                ))
            })?;
        let pubkey_hex = ack_pubkey.ok_or_else(|| {
            MultisigError::Signature(
                "ECDSA ack signature requires PSM public key (ack_pubkey not returned by server)"
                    .to_string(),
            )
        })?;
        build_ecdsa_signature_advice_entry(
            psm_commitment,
            tx_summary_commitment,
            &AccountSignature::EcdsaK256Keccak(ecdsa_sig),
            &pubkey_hex,
        )
    } else {
        let ack_signature = RpoFalconSignature::from_hex(&ack_sig_with_prefix).map_err(|e| {
            MultisigError::Signature(format!("failed to parse PSM ack signature: {}", e))
        })?;
        Ok(build_signature_advice_entry(
            psm_commitment,
            tx_summary_commitment,
            &AccountSignature::from(ack_signature),
            None,
        ))
    }
}
