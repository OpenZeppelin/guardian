use crate::error::{GuardianError, Result};

pub fn normalize_commitment_hex(commitment: &str) -> Result<String> {
    let normalized = commitment
        .strip_prefix("0x")
        .or_else(|| commitment.strip_prefix("0X"))
        .unwrap_or(commitment);

    if normalized.is_empty() {
        return Err(GuardianError::InvalidCommitment(
            "commitment cannot be empty".to_string(),
        ));
    }

    if !normalized.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(GuardianError::InvalidCommitment(
            "commitment must be hex-encoded".to_string(),
        ));
    }

    Ok(format!("0x{}", normalized))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_commitment_accepts_prefixed_hex() {
        let normalized = normalize_commitment_hex(
            "0xAABBCCDDEEFF00112233445566778899AABBCCDDEEFF00112233445566778899",
        )
        .expect("valid commitment should normalize");
        assert_eq!(
            normalized,
            "0xAABBCCDDEEFF00112233445566778899AABBCCDDEEFF00112233445566778899"
        );
    }

    #[test]
    fn normalize_commitment_accepts_non_prefixed_hex() {
        let normalized = normalize_commitment_hex(
            "AABBCCDDEEFF00112233445566778899AABBCCDDEEFF00112233445566778899",
        )
        .expect("valid commitment should normalize");
        assert_eq!(
            normalized,
            "0xAABBCCDDEEFF00112233445566778899AABBCCDDEEFF00112233445566778899"
        );
    }

    #[test]
    fn normalize_commitment_rejects_non_hex() {
        let err = normalize_commitment_hex("../../other_account/proposals/abc")
            .expect_err("non-hex commitment must fail");
        assert!(matches!(err, GuardianError::InvalidCommitment(_)));
    }
}
