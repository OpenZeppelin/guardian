use miden_objects::Word;
use miden_objects::crypto::dsa::ecdsa_k256_keccak::{
    PublicKey as EcdsaPublicKey, Signature as EcdsaSignature,
};
use miden_objects::crypto::dsa::rpo_falcon512::{PublicKey, Signature};
use miden_objects::utils::{Deserializable, Serializable};

/// Trait for converting types to hex strings with `0x` prefix
pub trait IntoHex {
    fn into_hex(self) -> String;
}

/// Trait for parsing types from hex strings (with or without `0x` prefix)
pub trait FromHex: Sized {
    fn from_hex(hex: &str) -> Result<Self, String>;
}

impl IntoHex for &PublicKey {
    fn into_hex(self) -> String {
        let mut pubkey_bytes = Vec::new();
        self.write_into(&mut pubkey_bytes);
        format!("0x{}", hex::encode(pubkey_bytes))
    }
}

impl IntoHex for PublicKey {
    fn into_hex(self) -> String {
        (&self).into_hex()
    }
}

impl FromHex for PublicKey {
    fn from_hex(hex: &str) -> Result<Self, String> {
        let hex_str = hex.strip_prefix("0x").unwrap_or(hex);
        let bytes = hex::decode(hex_str).map_err(|e| format!("Invalid public key hex: {e}"))?;
        PublicKey::read_from_bytes(&bytes)
            .map_err(|e| format!("Failed to deserialize public key: {e}"))
    }
}

impl IntoHex for Signature {
    fn into_hex(self) -> String {
        let signature_bytes = self.to_bytes();
        format!("0x{}", hex::encode(&signature_bytes))
    }
}

impl FromHex for Signature {
    fn from_hex(hex: &str) -> Result<Self, String> {
        let hex_str = hex.strip_prefix("0x").unwrap_or(hex);
        let bytes = hex::decode(hex_str).map_err(|e| format!("Invalid signature hex: {e}"))?;

        const EXPECTED_SIG_LEN: usize = 1524;
        if bytes.len() != EXPECTED_SIG_LEN {
            return Err(format!(
                "Signature must be exactly {EXPECTED_SIG_LEN} bytes, got {} bytes",
                bytes.len()
            ));
        }

        Signature::read_from_bytes(&bytes)
            .map_err(|e| format!("Failed to deserialize signature: {e}"))
    }
}

impl IntoHex for &EcdsaPublicKey {
    fn into_hex(self) -> String {
        let mut pubkey_bytes = Vec::new();
        self.write_into(&mut pubkey_bytes);
        format!("0x{}", hex::encode(pubkey_bytes))
    }
}

impl IntoHex for EcdsaPublicKey {
    fn into_hex(self) -> String {
        (&self).into_hex()
    }
}

impl FromHex for EcdsaPublicKey {
    fn from_hex(hex: &str) -> Result<Self, String> {
        let hex_str = hex.strip_prefix("0x").unwrap_or(hex);
        let bytes =
            hex::decode(hex_str).map_err(|e| format!("Invalid ECDSA public key hex: {e}"))?;
        EcdsaPublicKey::read_from_bytes(&bytes)
            .map_err(|e| format!("Failed to deserialize ECDSA public key: {e}"))
    }
}

impl IntoHex for EcdsaSignature {
    fn into_hex(self) -> String {
        let signature_bytes = self.to_bytes();
        format!("0x{}", hex::encode(&signature_bytes))
    }
}

impl FromHex for EcdsaSignature {
    fn from_hex(hex: &str) -> Result<Self, String> {
        let hex_str = hex.strip_prefix("0x").unwrap_or(hex);
        let bytes =
            hex::decode(hex_str).map_err(|e| format!("Invalid ECDSA signature hex: {e}"))?;
        EcdsaSignature::read_from_bytes(&bytes)
            .map_err(|e| format!("Failed to deserialize ECDSA signature: {e}"))
    }
}

impl IntoHex for Word {
    fn into_hex(self) -> String {
        format!("0x{}", hex::encode(self.as_bytes()))
    }
}

impl IntoHex for &Word {
    fn into_hex(self) -> String {
        format!("0x{}", hex::encode(self.as_bytes()))
    }
}

impl FromHex for Word {
    fn from_hex(hex: &str) -> Result<Self, String> {
        let hex_str = hex.strip_prefix("0x").unwrap_or(hex);
        let bytes = hex::decode(hex_str).map_err(|e| format!("Invalid word hex: {e}"))?;

        Word::read_from_bytes(&bytes).map_err(|e| format!("Failed to deserialize word: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;

    #[test]
    fn test_public_key_into_hex() {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();

        // Test reference implementation
        let hex1 = (&public_key).into_hex();
        assert!(hex1.starts_with("0x"));
        assert_eq!(hex1.len(), 2 + (897 * 2));

        // Test owned implementation
        let hex2 = public_key.into_hex();
        assert_eq!(hex1, hex2);
    }

    #[test]
    fn test_public_key_from_hex_roundtrip() {
        let secret_key = SecretKey::new();
        let original_pubkey = secret_key.public_key();

        // Convert to hex
        let hex = original_pubkey.into_hex();

        // Parse back from hex
        let parsed_pubkey = PublicKey::from_hex(&hex).expect("Failed to parse public key");

        // Verify roundtrip
        assert_eq!(hex, parsed_pubkey.into_hex());
    }

    #[test]
    fn test_public_key_from_hex_without_prefix() {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let hex_with_prefix = public_key.into_hex();

        // Remove 0x prefix
        let hex_without_prefix = hex_with_prefix.strip_prefix("0x").unwrap();

        // Both should parse successfully
        let pubkey1 = PublicKey::from_hex(&hex_with_prefix).unwrap();
        let pubkey2 = PublicKey::from_hex(hex_without_prefix).unwrap();

        assert_eq!(pubkey1.into_hex(), pubkey2.into_hex());
    }

    #[test]
    fn test_signature_into_hex() {
        use miden_objects::Word;
        let secret_key = SecretKey::new();
        let message = Word::from([1u32, 2, 3, 4]);
        let signature = secret_key.sign(message);

        let hex = signature.into_hex();
        assert!(hex.starts_with("0x"));
        assert_eq!(hex.len(), 2 + (1524 * 2));
    }

    #[test]
    fn test_signature_from_hex_roundtrip() {
        use miden_objects::Word;
        let secret_key = SecretKey::new();
        let message = Word::from([1u32, 2, 3, 4]);
        let original_sig = secret_key.sign(message);

        // Convert to hex
        let hex = original_sig.into_hex();

        // Parse back from hex
        let parsed_sig = Signature::from_hex(&hex).expect("Failed to parse signature");

        // Verify roundtrip
        assert_eq!(hex, parsed_sig.into_hex());
    }

    #[test]
    fn test_signature_from_hex_validates_length() {
        // Too short
        let result = Signature::from_hex("0x1234");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("1524 bytes"));
    }

    #[test]
    fn test_word_into_hex() {
        let word = Word::from([1u32, 2, 3, 4]);
        let hex = word.into_hex();
        assert!(hex.starts_with("0x"));
        // Word is 32 bytes (4 x 8-byte Felt values)
        assert_eq!(hex.len(), 2 + (32 * 2));
    }

    #[test]
    fn test_word_from_hex_roundtrip() {
        let original = Word::from([0xdeadbeefu32, 0xcafebabe, 0x12345678, 0x87654321]);
        let hex = original.into_hex();
        let parsed = Word::from_hex(&hex).expect("Failed to parse word");
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_word_from_hex_without_prefix() {
        let word = Word::from([1u32, 2, 3, 4]);
        let hex_with_prefix = word.into_hex();
        let hex_without_prefix = hex_with_prefix.strip_prefix("0x").unwrap();

        let word1 = Word::from_hex(&hex_with_prefix).unwrap();
        let word2 = Word::from_hex(hex_without_prefix).unwrap();
        assert_eq!(word1, word2);
    }

    #[test]
    fn test_word_reference_into_hex() {
        let word = Word::from([1u32, 2, 3, 4]);
        let hex1 = (&word).into_hex();
        let hex2 = word.into_hex();
        assert_eq!(hex1, hex2);
    }

    // --- ECDSA PublicKey tests ---

    #[test]
    fn test_ecdsa_public_key_into_hex() {
        use miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;
        let sk = EcdsaSecretKey::new();
        let pk = sk.public_key();

        let hex1 = (&pk).into_hex();
        assert!(hex1.starts_with("0x"));

        let hex2 = pk.into_hex();
        assert_eq!(hex1, hex2);
    }

    #[test]
    fn test_ecdsa_public_key_from_hex_roundtrip() {
        use miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;
        let sk = EcdsaSecretKey::new();
        let pk = sk.public_key();
        let hex = pk.into_hex();

        let parsed = EcdsaPublicKey::from_hex(&hex).expect("Failed to parse ECDSA public key");
        assert_eq!(hex, parsed.into_hex());
    }

    #[test]
    fn test_ecdsa_public_key_from_hex_without_prefix() {
        use miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;
        let sk = EcdsaSecretKey::new();
        let pk = sk.public_key();
        let hex_with_prefix = pk.into_hex();
        let hex_without_prefix = hex_with_prefix.strip_prefix("0x").unwrap();

        let pk1 = EcdsaPublicKey::from_hex(&hex_with_prefix).unwrap();
        let pk2 = EcdsaPublicKey::from_hex(hex_without_prefix).unwrap();
        assert_eq!(pk1.into_hex(), pk2.into_hex());
    }

    #[test]
    fn test_ecdsa_public_key_from_hex_invalid() {
        let result = EcdsaPublicKey::from_hex("0xinvalid");
        assert!(result.is_err());
    }

    // --- ECDSA Signature tests ---

    #[test]
    fn test_ecdsa_signature_into_hex() {
        use miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;
        let sk = EcdsaSecretKey::new();
        let message = Word::from([1u32, 2, 3, 4]);
        let sig = sk.sign(message);

        let hex = sig.into_hex();
        assert!(hex.starts_with("0x"));
    }

    #[test]
    fn test_ecdsa_signature_from_hex_roundtrip() {
        use miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;
        let sk = EcdsaSecretKey::new();
        let message = Word::from([1u32, 2, 3, 4]);
        let sig = sk.sign(message);

        let hex = sig.into_hex();
        let parsed = EcdsaSignature::from_hex(&hex).expect("Failed to parse ECDSA signature");
        assert_eq!(hex, parsed.into_hex());
    }

    #[test]
    fn test_ecdsa_signature_from_hex_invalid() {
        let result = EcdsaSignature::from_hex("0x1234");
        assert!(result.is_err());
    }

    // --- Error case tests ---

    #[test]
    fn test_falcon_public_key_from_hex_invalid() {
        let result = PublicKey::from_hex("0xnotvalidhex!");
        assert!(result.is_err());
    }

    #[test]
    fn test_falcon_public_key_from_hex_wrong_length() {
        let result = PublicKey::from_hex("0xabcd");
        assert!(result.is_err());
    }

    #[test]
    fn test_word_from_hex_invalid() {
        let result = Word::from_hex("0xnotvalidhex!");
        assert!(result.is_err());
    }

    #[test]
    fn test_word_from_hex_wrong_length() {
        let result = Word::from_hex("0xabcd");
        assert!(result.is_err());
    }
}
