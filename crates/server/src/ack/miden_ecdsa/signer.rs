use super::backend::EcdsaSignerBackend;
use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, Result};
use guardian_shared::FromJson;
use miden_keystore::ecdsa_commitment_hex;
use miden_protocol::{transaction::TransactionSummary, utils::serde::Serializable};
use std::sync::Arc;

#[derive(Clone)]
pub struct MidenEcdsaSigner {
    backend: Arc<dyn EcdsaSignerBackend>,
    pubkey_hex: String,
    commitment_hex: String,
}

impl MidenEcdsaSigner {
    pub fn new(backend: Arc<dyn EcdsaSignerBackend>) -> Self {
        let public_key = backend.public_key();
        let pubkey_hex = format!("0x{}", hex::encode(public_key.to_bytes()));
        let commitment_hex = ecdsa_commitment_hex(public_key);
        Self {
            backend,
            pubkey_hex,
            commitment_hex,
        }
    }

    pub(crate) fn pubkey_hex(&self) -> String {
        self.pubkey_hex.clone()
    }

    pub(crate) fn commitment_hex(&self) -> String {
        self.commitment_hex.clone()
    }

    pub(crate) async fn ack_delta(&self, mut delta: DeltaObject) -> Result<DeltaObject> {
        let tx_summary = TransactionSummary::from_json(&delta.delta_payload).map_err(|e| {
            GuardianError::InvalidDelta(format!("Failed to deserialize TransactionSummary: {e}"))
        })?;

        let tx_commitment = tx_summary.to_commitment();
        let signature = self.backend.sign(tx_commitment).await?;
        delta.ack_sig = hex::encode(signature.to_bytes());
        Ok(delta)
    }
}
