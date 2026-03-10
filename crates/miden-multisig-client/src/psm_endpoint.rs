use miden_protocol::Word;
use private_state_manager_client::PsmClient;

use crate::error::{MultisigError, Result};
use crate::keystore::word_from_hex;
use crate::transaction::word_to_hex;

pub(crate) async fn verify_endpoint_commitment(
    endpoint: &str,
    expected_commitment: Word,
) -> Result<()> {
    let mut client = PsmClient::connect(endpoint).await.map_err(|e| {
        MultisigError::PsmConnection(format!(
            "failed to connect to PSM endpoint {}: {}",
            endpoint, e
        ))
    })?;

    let endpoint_commitment_hex = client.get_pubkey().await.map_err(|e| {
        MultisigError::PsmServer(format!(
            "failed to get pubkey from PSM endpoint {}: {}",
            endpoint, e
        ))
    })?;

    let endpoint_commitment =
        word_from_hex(&endpoint_commitment_hex).map_err(MultisigError::HexDecode)?;

    ensure_commitment_match(endpoint, expected_commitment, endpoint_commitment)
}

pub(crate) fn ensure_commitment_match(
    endpoint: &str,
    expected_commitment: Word,
    endpoint_commitment: Word,
) -> Result<()> {
    if endpoint_commitment == expected_commitment {
        return Ok(());
    }

    Err(MultisigError::InvalidConfig(format!(
        "refusing to use PSM endpoint {}: endpoint pubkey commitment {} does not match expected {}",
        endpoint,
        word_to_hex(&endpoint_commitment),
        word_to_hex(&expected_commitment)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_protocol::Word;

    #[test]
    fn ensure_commitment_match_accepts_equal_commitments() {
        let commitment = Word::from([1u32, 2, 3, 4]);
        let result = ensure_commitment_match("http://localhost:50051", commitment, commitment);
        assert!(result.is_ok());
    }

    #[test]
    fn ensure_commitment_match_rejects_mismatch() {
        let expected = Word::from([1u32, 2, 3, 4]);
        let actual = Word::from([5u32, 6, 7, 8]);
        let error = ensure_commitment_match("http://localhost:50051", expected, actual)
            .expect_err("expected mismatch error");

        let message = error.to_string();
        assert!(message.contains("refusing to use PSM endpoint"));
        assert!(message.contains("does not match expected"));
    }
}
