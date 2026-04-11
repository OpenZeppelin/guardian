use miden_protocol::Word;
use miden_protocol::crypto::dsa::falcon512_poseidon2::{PublicKey, SecretKey, Signature};

/// Generate a new Falcon Poseidon2-512 key pair
///
/// # Returns
/// A tuple of (SecretKey, PublicKey)
pub fn generate_keypair() -> (SecretKey, PublicKey) {
    let secret_key = SecretKey::new();
    let public_key = secret_key.public_key();
    (secret_key, public_key)
}

/// Sign a message using a Falcon Poseidon2-512 secret key
///
/// # Arguments
/// * `secret_key` - The secret key to sign with
/// * `message` - The message as a Word (4 field elements)
///
/// # Returns
/// A Falcon Poseidon2-512 signature
pub fn sign_message(secret_key: &SecretKey, message: Word) -> Signature {
    secret_key.sign(message)
}

/// Verify a Falcon Poseidon2-512 signature
///
/// # Arguments
/// * `public_key` - The public key to verify against
/// * `message` - The message Word that was signed
/// * `signature` - The signature to verify
///
/// # Returns
/// `true` if the signature is valid, `false` otherwise
pub fn verify_signature(public_key: &PublicKey, message: Word, signature: &Signature) -> bool {
    public_key.verify(message, signature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_protocol::Felt;

    #[test]
    fn test_keypair_generation() {
        let (_secret_key, _public_key) = generate_keypair();
        // If we got here without panicking, keypair generation succeeded
    }

    #[test]
    fn test_sign_and_verify() {
        // Generate a keypair
        let (secret_key, public_key) = generate_keypair();

        // Create a test message (using a Word of 4 field elements)
        let message: Word = [Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)].into();

        // Sign the message
        let signature = sign_message(&secret_key, message);

        // Verify the signature
        let is_valid = verify_signature(&public_key, message, &signature);
        assert!(is_valid, "Signature should be valid");
    }

    #[test]
    fn test_verify_with_wrong_message() {
        // Generate a keypair
        let (secret_key, public_key) = generate_keypair();

        // Create and sign a message
        let message: Word = [Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)].into();
        let signature = sign_message(&secret_key, message);

        // Try to verify with a different message
        let wrong_message: Word = [Felt::new(5), Felt::new(6), Felt::new(7), Felt::new(8)].into();
        let is_valid = verify_signature(&public_key, wrong_message, &signature);
        assert!(!is_valid, "Signature should be invalid for wrong message");
    }

    #[test]
    fn test_verify_with_wrong_key() {
        // Generate two keypairs
        let (secret_key1, _public_key1) = generate_keypair();
        let (_secret_key2, public_key2) = generate_keypair();

        // Sign with first key
        let message: Word = [Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)].into();
        let signature = sign_message(&secret_key1, message);

        // Try to verify with second public key
        let is_valid = verify_signature(&public_key2, message, &signature);
        assert!(
            !is_valid,
            "Signature should be invalid for wrong public key"
        );
    }
}
