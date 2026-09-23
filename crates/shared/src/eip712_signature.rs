use miden_protocol::crypto::dsa::ecdsa_k256_keccak::Signature;
use miden_protocol::utils::serde::Deserializable;

/// Parses Ethereum `r || s || v` signatures for Miden's ECDSA verifier.
pub trait FromEip712Hex: Sized {
    fn from_eip712_hex(signature_hex: &str) -> Result<Self, String>;
}

impl FromEip712Hex for Signature {
    fn from_eip712_hex(signature_hex: &str) -> Result<Self, String> {
        let mut bytes = hex::decode(signature_hex.trim_start_matches("0x"))
            .map_err(|e| format!("invalid EIP-712 signature hex: {e}"))?;
        if bytes.len() != 65 {
            return Err("EIP-712 signature must be 65 bytes".to_string());
        }
        bytes[64] = match bytes[64] {
            0 | 1 => bytes[64],
            27 | 28 => bytes[64] - 27,
            _ => return Err("invalid EIP-712 recovery ID".to_string()),
        };
        Signature::read_from_bytes(&bytes).map_err(|e| format!("invalid EIP-712 signature: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_protocol::utils::serde::Serializable;

    #[rstest::rstest]
    #[case(0)]
    #[case(1)]
    fn accepts_ethereum_recovery_bytes(#[case] recovery: u8) {
        let mut bytes = [0u8; 65];
        bytes[0] = 1;
        bytes[32] = 2;
        bytes[64] = recovery + 27;
        let parsed = Signature::from_eip712_hex(&hex::encode(bytes)).unwrap();
        assert_eq!(parsed.to_bytes()[64], recovery);
    }

    #[test]
    fn rejects_invalid_recovery_byte_and_length() {
        let mut bytes = [0u8; 65];
        bytes[64] = 29;
        assert!(Signature::from_eip712_hex(&hex::encode(bytes)).is_err());
        assert!(Signature::from_eip712_hex("0x00").is_err());
    }
}
