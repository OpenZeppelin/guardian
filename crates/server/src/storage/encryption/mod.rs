//! Application-layer encryption of sensitive storage payloads.
//!
//! Sensitive payloads (account state, delta/proposal payloads) are
//! authenticated-encrypted into a self-describing [`envelope::Envelope`] before
//! they reach the concrete backend and decrypted on read, so layers above the
//! storage boundary see unchanged objects. Routing/index fields stay plaintext.

pub(crate) mod cipher;
pub(crate) mod decorator;
pub(crate) mod envelope;
pub(crate) mod key_provider;
pub(crate) mod marker;

/// Whether a storage read error describes a single record that cannot be
/// read on its own — a payload that is not an envelope, an unsupported
/// envelope, a bad nonce, a failed AEAD check, or a row sealed under a
/// key id this deployment no longer holds — as opposed to a systemic
/// failure (key store unavailable, malformed key material, I/O).
/// Matches the `Display` strings of [`cipher::CipherError`] and
/// [`key_provider::KeyProviderError`]; the test below constructs every
/// variant so a reworded message cannot silently flip a classification.
/// Callers that aggregate across many records report one bad row as
/// explicit coverage while treating anything else as a reason to keep
/// their previous result (a walk in which *no* row decodes is escalated
/// to systemic by the caller regardless of this classification).
pub fn is_record_corruption(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("not an encryption envelope")
        || lower.contains("unsupported envelope")
        || lower.contains("envelope nonce is malformed")
        || lower.contains("decryption failed")
        || (lower.contains("storage encryption key id") && lower.contains("is not available"))
}

#[cfg(test)]
mod classification_tests {
    use super::cipher::CipherError;
    use super::is_record_corruption;
    use super::key_provider::KeyProviderError;

    #[test]
    fn every_cipher_error_variant_is_classified_by_construction() {
        let per_record = [
            CipherError::NotAnEnvelope,
            CipherError::UnsupportedVersion(7),
            CipherError::UnsupportedAlgorithm("ROT13".into()),
            CipherError::InvalidNonce,
            CipherError::DecryptionFailed,
            CipherError::KeyProvider(KeyProviderError::UnknownKeyId("retired-kid".into())),
        ];
        for err in per_record {
            assert!(
                is_record_corruption(&err.to_string()),
                "{err} should be per-record"
            );
        }
        let systemic = [
            CipherError::EncryptionFailed,
            CipherError::KeyProvider(KeyProviderError::KeyStoreUnavailable("kms timeout".into())),
            CipherError::KeyProvider(KeyProviderError::MultipleKeySources),
            CipherError::KeyProvider(KeyProviderError::InvalidKeyEncoding),
            CipherError::KeyProvider(KeyProviderError::InvalidKeyLength),
            CipherError::KeyProvider(KeyProviderError::MalformedSecret),
        ];
        for err in systemic {
            assert!(
                !is_record_corruption(&err.to_string()),
                "{err} should be systemic"
            );
        }
    }
}
