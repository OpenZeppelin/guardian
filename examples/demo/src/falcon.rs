use std::sync::Arc;

use miden_client::keystore::FilesystemKeyStore;
use miden_client::Serializable;
use miden_objects::account::auth::AuthSecretKey;
use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
use rand::rngs::StdRng;

pub fn generate_falcon_keypair(
    keystore: Arc<FilesystemKeyStore<StdRng>>,
) -> Result<(String, SecretKey), String> {
    let secret_key = SecretKey::new();
    let public_key = secret_key.public_key();

    let commitment = public_key.to_commitment();
    let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));

    let auth_key = AuthSecretKey::RpoFalcon512(secret_key.clone());
    keystore
        .add_key(&auth_key)
        .map_err(|e| format!("Failed to add key to keystore: {}", e))?;

    Ok((commitment_hex, secret_key))
}
