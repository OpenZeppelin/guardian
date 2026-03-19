use miden_client::auth::AuthSecretKey;
use miden_client::crypto::rpo_falcon512::SecretKey;
use miden_client::keystore::FilesystemKeyStore;
use miden_client::Serializable;

/// Generate a Falcon keypair and return (full_pubkey_hex, commitment_hex, secret_key)
pub fn generate_falcon_keypair(keystore: &FilesystemKeyStore) -> (String, String, SecretKey) {
    // Generate a new secret key
    let secret_key = SecretKey::new();
    let auth_secret_key = AuthSecretKey::Falcon512Rpo(secret_key.clone());

    // Add it to the keystore
    keystore
        .add_key(&auth_secret_key)
        .expect("Failed to add key to keystore");

    // Get the public key and commitment
    let actual_pubkey = secret_key.public_key();
    let actual_commitment = actual_pubkey.to_commitment();

    // Verify we can retrieve it
    let retrieved_key = keystore
        .get_key(actual_commitment.into())
        .expect("Failed to get key")
        .expect("Key not found in keystore");

    // Verify the retrieved key matches
    let AuthSecretKey::Falcon512Rpo(retrieved_secret) = retrieved_key else {
        panic!("Expected Falcon512Rpo key but got different variant");
    };
    assert_eq!(
        retrieved_secret.public_key().to_commitment(),
        actual_commitment,
        "Retrieved key doesn't match!"
    );

    // Return both full public key (for auth) and commitment (for account storage)
    use guardian_shared::hex::IntoHex;
    let full_pubkey_hex = (&actual_pubkey).into_hex();
    let commitment_hex = format!("0x{}", hex::encode(actual_commitment.to_bytes()));

    (full_pubkey_hex, commitment_hex, secret_key)
}
