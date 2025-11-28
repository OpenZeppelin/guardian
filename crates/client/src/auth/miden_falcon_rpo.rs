//! Falcon signature-based authentication using RPO hashing.

use miden_objects::account::AccountId;
use miden_objects::crypto::dsa::rpo_falcon512::{PublicKey, SecretKey, Signature};
use miden_objects::crypto::hash::rpo::Rpo256;
use miden_objects::utils::{Deserializable, Serializable};
use miden_objects::{Felt, FieldElement, Word};
use private_state_manager_shared::hex::{FromHex, IntoHex};

/// A signer that uses Falcon signatures with RPO hashing.
///
/// This is the primary authentication mechanism for PSM requests,
/// compatible with Miden's native signature scheme.
pub struct FalconRpoSigner {
    secret_key: SecretKey,
    public_key: PublicKey,
}

impl FalconRpoSigner {
    /// Creates a new signer from a Falcon secret key.
    pub fn new(secret_key: SecretKey) -> Self {
        let public_key = secret_key.public_key();
        Self {
            secret_key,
            public_key,
        }
    }

    /// Returns the hex-encoded public key.
    pub fn public_key_hex(&self) -> String {
        (&self.public_key).into_hex()
    }

    /// Signs an account ID and returns the hex-encoded signature.
    pub fn sign_account_id(&self, account_id: &AccountId) -> String {
        let message = account_id.into_word();
        let signature = self.secret_key.sign(message);
        signature.into_hex()
    }
}

/// Trait for converting types to a [`Word`] for signing.
pub trait IntoWord {
    /// Converts this value into a Word suitable for signing.
    fn into_word(self) -> Word;
}

impl IntoWord for AccountId {
    fn into_word(self) -> Word {
        let account_id_felts: [Felt; 2] = (self).into();

        let message_elements = vec![
            account_id_felts[0],
            account_id_felts[1],
            Felt::ZERO,
            Felt::ZERO,
        ];

        Rpo256::hash_elements(&message_elements)
    }
}

/// Verifies a signature using commitment-based authentication.
///
/// This function verifies that a signature was created by a key whose
/// commitment matches the expected server commitment.
pub fn verify_commitment_signature(
    commitment_hex: &str,
    server_commitment_hex: &str,
    signature_hex: &str,
) -> Result<bool, String> {
    let message = commitment_hex.hex_into_word()?;
    let signature = Signature::from_hex(signature_hex)?;

    // Extract the public key from the signature
    let pubkey = signature.public_key();

    // Compute the commitment of the extracted public key
    let sig_pubkey_commitment = pubkey.to_commitment();
    let sig_commitment_hex = format!("0x{}", hex::encode(sig_pubkey_commitment.to_bytes()));

    // Check if the computed commitment matches the expected server commitment
    if sig_commitment_hex != server_commitment_hex {
        return Ok(false);
    }

    // Verify the signature cryptographically
    Ok(pubkey.verify(message, &signature))
}

/// Trait for parsing hex strings into [`Word`] values.
pub trait HexIntoWord {
    /// Parses this hex string into a Word.
    fn hex_into_word(self) -> Result<Word, String>;
}

impl HexIntoWord for &str {
    fn hex_into_word(self) -> Result<Word, String> {
        let commitment_hex = self.strip_prefix("0x").unwrap_or(self);

        let bytes =
            hex::decode(commitment_hex).map_err(|e| format!("Invalid commitment hex: {e}"))?;

        if bytes.len() != 32 {
            return Err(format!("Commitment must be 32 bytes, got {}", bytes.len()));
        }

        // Use Word::read_from_bytes to deserialize the commitment correctly
        Word::read_from_bytes(&bytes)
            .map_err(|e| format!("Failed to deserialize Word from bytes: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_falcon_signer_creates_valid_signature() {
        use miden_objects::utils::Deserializable;

        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let signer = FalconRpoSigner::new(secret_key);

        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let signature_hex = signer.sign_account_id(&account_id);

        // Verify signature format
        assert!(signature_hex.starts_with("0x"));

        // Verify the signature is valid
        let sig_bytes = hex::decode(signature_hex.strip_prefix("0x").unwrap()).unwrap();
        let signature = Signature::read_from_bytes(&sig_bytes).unwrap();

        // Create the message digest that was signed (using the same method as sign_account_id)
        let message = account_id.into_word();

        // Verify signature with public key
        assert!(
            public_key.verify(message, &signature),
            "Signature verification failed"
        );
    }

    #[test]
    fn test_public_key_from_hex_roundtrip() {
        let secret_key = SecretKey::new();
        let original_pubkey = secret_key.public_key();

        // Convert to hex
        let hex = original_pubkey.into_hex();

        // Parse back from hex
        let parsed_pubkey = PublicKey::from_hex(&hex).expect("Failed to parse public key from hex");

        // Verify they produce the same hex representation
        let parsed_hex = parsed_pubkey.into_hex();
        assert_eq!(
            hex, parsed_hex,
            "Roundtrip should produce identical public key"
        );
    }

    #[test]
    fn test_signature_from_hex_roundtrip() {
        let secret_key = SecretKey::new();
        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let message = account_id.into_word();
        let original_sig = secret_key.sign(message);

        // Convert to hex
        let hex = original_sig.into_hex();

        // Parse back from hex
        let parsed_sig = Signature::from_hex(&hex).expect("Failed to parse signature from hex");

        // Verify they produce the same hex representation
        let parsed_hex = parsed_sig.into_hex();
        assert_eq!(
            hex, parsed_hex,
            "Roundtrip should produce identical signature"
        );
    }

    #[test]
    fn test_from_hex_without_prefix() {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let hex_with_prefix = public_key.into_hex();

        // Remove 0x prefix
        let hex_without_prefix = hex_with_prefix.strip_prefix("0x").unwrap();

        // Both should parse successfully
        let pubkey1 = PublicKey::from_hex(&hex_with_prefix).unwrap();
        let pubkey2 = PublicKey::from_hex(hex_without_prefix).unwrap();

        assert_eq!(
            pubkey1.into_hex(),
            pubkey2.into_hex(),
            "Parsing with and without 0x prefix should produce same result"
        );
    }
}
