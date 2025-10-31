use std::sync::Arc;

use miden_client::keystore::FilesystemKeyStore;
use miden_client::{Deserializable, Serializable};
use miden_objects::account::AuthSecretKey;
use miden_objects::crypto::dsa::rpo_falcon512::{PublicKey, SecretKey};
use rand::rngs::StdRng;

pub fn generate_falcon_keypair(
    keystore: Arc<FilesystemKeyStore<StdRng>>,
) -> Result<(String, String, SecretKey), String> {
    let secret_key = SecretKey::new();
    let public_key = secret_key.public_key();

    let full_pubkey_bytes = (&public_key).to_bytes();
    let full_pubkey_hex = format!("0x{}", hex::encode(full_pubkey_bytes));

    let commitment = public_key.to_commitment();
    let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));

    let auth_key = AuthSecretKey::RpoFalcon512(secret_key.clone());
    keystore
        .add_key(&auth_key)
        .map_err(|e| format!("Failed to add key to keystore: {}", e))?;

    Ok((full_pubkey_hex, commitment_hex, secret_key))
}

/// Derive commitment from public key hex
pub fn pubkey_to_commitment(pubkey_hex: &str) -> Result<String, String> {
    let pubkey_hex = pubkey_hex.strip_prefix("0x").unwrap_or(pubkey_hex);
    let pubkey_bytes = hex::decode(pubkey_hex)
        .map_err(|e| format!("Invalid public key hex: {}", e))?;

    let public_key = PublicKey::read_from_bytes(&pubkey_bytes)
        .map_err(|e| format!("Failed to deserialize public key: {:?}", e))?;

    let commitment = public_key.to_commitment();
    let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));

    Ok(commitment_hex)
}
