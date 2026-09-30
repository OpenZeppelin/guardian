use guardian_client::GuardianClient;
use guardian_shared::SignatureScheme;
use miden_protocol::Word;

use crate::error::{MultisigError, Result};
use crate::keystore::word_from_hex;
use crate::transaction::word_to_hex;

/// Confirms the GUARDIAN at `endpoint` is the one `expected_commitment` names.
///
/// The scheme is required, not optional: GUARDIAN holds one acknowledgement
/// identity per signature scheme and an account's guardian slot holds the one
/// matching its own. Asking without a scheme returns GUARDIAN's default, so a
/// non-default account could never satisfy this check whatever it passed.
pub(crate) async fn verify_endpoint_commitment(
    endpoint: &str,
    expected_commitment: Word,
    scheme: SignatureScheme,
) -> Result<()> {
    let mut client = GuardianClient::connect(endpoint).await.map_err(|e| {
        MultisigError::GuardianConnection(format!(
            "failed to connect to GUARDIAN endpoint {}: {}",
            endpoint, e
        ))
    })?;

    let (endpoint_commitment_hex, _raw_pubkey) = client
        .get_pubkey(Some(scheme.as_str()))
        .await
        .map_err(|e| {
            MultisigError::GuardianServer(format!(
                "failed to get pubkey from GUARDIAN endpoint {}: {}",
                endpoint, e
            ))
        })?;

    let endpoint_commitment =
        word_from_hex(&endpoint_commitment_hex).map_err(MultisigError::HexDecode)?;

    ensure_commitment_match(endpoint, scheme, expected_commitment, endpoint_commitment)
}

/// The scheme is named in the failure: the commitment served depends on which
/// acknowledgement identity was asked for, so a mismatch is as likely to mean
/// the wrong identity was queried as the wrong endpoint.
pub(crate) fn ensure_commitment_match(
    endpoint: &str,
    scheme: SignatureScheme,
    expected_commitment: Word,
    endpoint_commitment: Word,
) -> Result<()> {
    if endpoint_commitment == expected_commitment {
        return Ok(());
    }

    Err(MultisigError::InvalidConfig(format!(
        "refusing to use GUARDIAN endpoint {}: its {} acknowledgement commitment {} does not match expected {}",
        endpoint,
        scheme.as_str(),
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
        let result = ensure_commitment_match(
            "http://localhost:50051",
            SignatureScheme::Falcon,
            commitment,
            commitment,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn ensure_commitment_match_rejects_mismatch_and_names_the_queried_scheme() {
        let expected = Word::from([1u32, 2, 3, 4]);
        let actual = Word::from([5u32, 6, 7, 8]);
        let error = ensure_commitment_match(
            "http://localhost:50051",
            SignatureScheme::Ecdsa,
            expected,
            actual,
        )
        .expect_err("expected mismatch error");

        let message = error.to_string();
        assert!(message.contains("refusing to use GUARDIAN endpoint"));
        assert!(message.contains("does not match expected"));
        assert!(
            message.contains(SignatureScheme::Ecdsa.as_str()),
            "the mismatch must name the scheme that was queried: {message}"
        );
    }
}
