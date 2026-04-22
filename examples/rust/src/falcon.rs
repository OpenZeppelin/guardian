use miden_client::keystore::FilesystemKeyStore;
use miden_client::Serializable;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;

/// Generate a Falcon keypair and return (full_pubkey_hex, commitment_hex, secret_key)
pub fn generate_falcon_keypair(_keystore: &FilesystemKeyStore) -> (String, String, SecretKey) {
    let secret_key = SecretKey::new();

    let actual_pubkey = secret_key.public_key();
    let actual_commitment = actual_pubkey.to_commitment();

    use guardian_shared::hex::IntoHex;
    let full_pubkey_hex = (&actual_pubkey).into_hex();
    let commitment_hex = format!("0x{}", hex::encode(actual_commitment.to_bytes()));

    (full_pubkey_hex, commitment_hex, secret_key)
}
