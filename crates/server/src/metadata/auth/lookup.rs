//! Authentication helpers for the `/state/lookup` endpoint.
//!
//! Bypasses [`Auth::compute_signer_commitment`] (which accepts a raw 32-byte
//! commitment as a pubkey stand-in): on lookup, accepting a commitment as
//! `x-pubkey` would let anyone who learned a commitment off-chain prove an
//! "identity" against it without holding the private key, collapsing
//! proof-of-possession. `parse_lookup_pubkey` enforces a real Falcon or
//! ECDSA public-key encoding.

use guardian_shared::hex::FromHex;
use guardian_shared::lookup_auth_message::LookupAuthMessage;
use miden_protocol::Word;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{
    PublicKey as EcdsaPublicKey, Signature as EcdsaSignature,
};
use miden_protocol::crypto::dsa::falcon512_poseidon2::{
    PublicKey as FalconPublicKey, Signature as FalconSignature,
};
use miden_protocol::utils::serde::Deserializable;

/// Length, in bytes, of a Miden public-key commitment. Inputs of exactly this
/// length are rejected by [`parse_lookup_pubkey`] to block the
/// commitment-as-pubkey shortcut accepted by [`Auth::compute_signer_commitment`].
const COMMITMENT_BYTE_LEN: usize = 32;

/// A parsed public key suitable for the lookup endpoint.
///
/// This type is intentionally constructed only via [`parse_lookup_pubkey`]
/// (or the `Falcon`/`Ecdsa` variants directly in test code) so that any value
/// flowing through the lookup verification path is guaranteed to come from a
/// real public-key encoding.
#[derive(Debug)]
pub enum LookupPublicKey {
    Falcon(FalconPublicKey),
    Ecdsa(EcdsaPublicKey),
}

/// Parse a `0x`-prefixed hex string strictly as a Miden Falcon or Miden ECDSA
/// public key encoding.
///
/// Rejects:
/// - Inputs that fail hex decoding (`Err` with "invalid hex").
/// - Inputs of exactly 32 bytes (the commitment-as-pubkey alias accepted by
///   [`Auth::compute_signer_commitment`] is explicitly disallowed here).
/// - Inputs that parse as neither Falcon nor ECDSA.
pub fn parse_lookup_pubkey(pubkey_hex: &str) -> Result<LookupPublicKey, String> {
    let trimmed = pubkey_hex.trim_start_matches("0x").trim_start_matches("0X");
    let bytes = hex::decode(trimmed).map_err(|e| format!("invalid public key hex: {e}"))?;

    if bytes.len() == COMMITMENT_BYTE_LEN {
        return Err(
            "lookup endpoint requires a full Falcon or ECDSA public key, not a 32-byte commitment"
                .to_string(),
        );
    }

    if let Ok(falcon_key) = FalconPublicKey::from_hex(pubkey_hex) {
        return Ok(LookupPublicKey::Falcon(falcon_key));
    }

    if let Ok(ecdsa_key) = EcdsaPublicKey::read_from_bytes(&bytes) {
        return Ok(LookupPublicKey::Ecdsa(ecdsa_key));
    }

    Err("public key did not parse as Falcon or ECDSA".to_string())
}

/// Derive the `0x`-prefixed lowercase hex commitment from a parsed public key.
///
/// Mirrors the format used everywhere else in `account_metadata.auth`
/// (`format!("0x{}", hex::encode(commitment.to_bytes()))`), so the result can
/// be compared directly to entries in `cosigner_commitments`.
pub fn commitment_of(pk: &LookupPublicKey) -> String {
    let commitment: Word = match pk {
        LookupPublicKey::Falcon(key) => key.to_commitment(),
        LookupPublicKey::Ecdsa(key) => key.to_commitment(),
    };
    format!("0x{}", hex::encode(commitment.as_bytes()))
}

/// Verify a `0x`-prefixed hex signature over the [`LookupAuthMessage`] digest
/// for `(timestamp_ms, key_commitment)`.
///
/// Returns `Ok(())` on a valid signature; any other outcome (parse failure,
/// signature mismatch) yields a string error suitable for mapping to
/// `GuardianError::AuthenticationFailed`.
pub fn verify_lookup_signature(
    pk: &LookupPublicKey,
    timestamp_ms: i64,
    key_commitment: Word,
    signature_hex: &str,
) -> Result<(), String> {
    let digest = LookupAuthMessage::new(timestamp_ms, key_commitment).to_word();

    match pk {
        LookupPublicKey::Falcon(public_key) => {
            let signature = FalconSignature::from_hex(signature_hex)
                .map_err(|e| format!("invalid Falcon signature: {e}"))?;
            if public_key.verify(digest, &signature) {
                Ok(())
            } else {
                Err("Falcon signature verification failed".to_string())
            }
        }
        LookupPublicKey::Ecdsa(public_key) => {
            let bytes = hex::decode(signature_hex.trim_start_matches("0x"))
                .map_err(|e| format!("invalid ECDSA signature hex: {e}"))?;
            let signature = EcdsaSignature::read_from_bytes(&bytes)
                .map_err(|e| format!("failed to deserialize ECDSA signature: {e}"))?;
            if public_key.verify(digest, &signature) {
                Ok(())
            } else {
                Err("ECDSA signature verification failed".to_string())
            }
        }
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use guardian_shared::auth_request_message::AuthRequestMessage;
    use guardian_shared::auth_request_payload::AuthRequestPayload;
    use guardian_shared::hex::IntoHex;
    use miden_protocol::Word;
    use miden_protocol::account::AccountId;
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;
    use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey as FalconSecretKey;
    use miden_protocol::utils::serde::Serializable;

    fn falcon_pubkey_hex(secret: &FalconSecretKey) -> String {
        secret.public_key().into_hex()
    }

    fn ecdsa_pubkey_hex(secret: &EcdsaSecretKey) -> String {
        format!("0x{}", hex::encode(secret.public_key().to_bytes()))
    }

    fn commitment_hex_word(word: Word) -> String {
        format!("0x{}", hex::encode(word.as_bytes()))
    }

    #[test]
    fn parse_lookup_pubkey_accepts_falcon_encoding() {
        let secret = FalconSecretKey::new();
        let parsed = parse_lookup_pubkey(&falcon_pubkey_hex(&secret)).expect("parse Falcon");
        match parsed {
            LookupPublicKey::Falcon(_) => {}
            other => panic!("expected Falcon, got {other:?}"),
        }
    }

    #[test]
    fn parse_lookup_pubkey_accepts_ecdsa_encoding() {
        let secret = EcdsaSecretKey::new();
        let parsed = parse_lookup_pubkey(&ecdsa_pubkey_hex(&secret)).expect("parse ECDSA");
        match parsed {
            LookupPublicKey::Ecdsa(_) => {}
            other => panic!("expected ECDSA, got {other:?}"),
        }
    }

    #[test]
    fn parse_lookup_pubkey_rejects_32_byte_commitment_alias() {
        // 64 hex chars == 32 bytes; this is exactly what
        // Auth::compute_signer_commitment passes through but lookup must reject.
        let raw_commitment = "0x".to_string() + &"ab".repeat(32);
        let err = parse_lookup_pubkey(&raw_commitment).expect_err("must reject 32-byte input");
        assert!(
            err.contains("32-byte commitment"),
            "error message must call out the 32-byte rejection: {err}"
        );
    }

    #[test]
    fn parse_lookup_pubkey_rejects_invalid_hex() {
        let err = parse_lookup_pubkey("0xZZ").expect_err("must reject invalid hex");
        assert!(err.contains("invalid public key hex"), "{err}");
    }

    #[test]
    fn parse_lookup_pubkey_rejects_unparseable_bytes() {
        // Length not 32, not a valid Falcon or ECDSA encoding.
        let bogus = "0x".to_string() + &"ff".repeat(48);
        let err = parse_lookup_pubkey(&bogus).expect_err("must reject unparseable input");
        assert!(err.contains("did not parse as Falcon or ECDSA"), "{err}");
    }

    #[test]
    fn commitment_of_matches_metadata_format_falcon() {
        let secret = FalconSecretKey::new();
        let public = secret.public_key();
        let parsed = parse_lookup_pubkey(&falcon_pubkey_hex(&secret)).expect("parse Falcon");
        let derived = commitment_of(&parsed);
        let expected = commitment_hex_word(public.to_commitment());
        assert_eq!(derived, expected);
    }

    #[test]
    fn commitment_of_matches_metadata_format_ecdsa() {
        let secret = EcdsaSecretKey::new();
        let public = secret.public_key();
        let parsed = parse_lookup_pubkey(&ecdsa_pubkey_hex(&secret)).expect("parse ECDSA");
        let derived = commitment_of(&parsed);
        let expected = commitment_hex_word(public.to_commitment());
        assert_eq!(derived, expected);
    }

    #[test]
    fn verify_lookup_signature_falcon_happy_path() {
        let secret = FalconSecretKey::new();
        let public = secret.public_key();
        let key_commitment = public.to_commitment();
        let timestamp = 1_700_000_000_000i64;

        let digest = LookupAuthMessage::new(timestamp, key_commitment).to_word();
        let signature = secret.sign(digest);
        let signature_hex = signature.into_hex();

        let parsed = parse_lookup_pubkey(&falcon_pubkey_hex(&secret)).expect("parse Falcon");
        verify_lookup_signature(&parsed, timestamp, key_commitment, &signature_hex)
            .expect("valid signature must verify");
    }

    #[test]
    fn verify_lookup_signature_falcon_rejects_tampered_signature() {
        let secret = FalconSecretKey::new();
        let public = secret.public_key();
        let key_commitment = public.to_commitment();
        let timestamp = 1_700_000_000_000i64;

        let digest = LookupAuthMessage::new(timestamp, key_commitment).to_word();
        let signature = secret.sign(digest);
        let mut bytes = signature.to_bytes();
        // Flip a byte in the middle to corrupt the signature.
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x01;
        let tampered_hex = format!("0x{}", hex::encode(&bytes));

        let parsed = parse_lookup_pubkey(&falcon_pubkey_hex(&secret)).expect("parse Falcon");
        let err = verify_lookup_signature(&parsed, timestamp, key_commitment, &tampered_hex)
            .expect_err("tampered signature must be rejected");
        // Either a parse failure or an explicit "verification failed" is acceptable;
        // both lead to AuthenticationFailed at the service layer.
        assert!(
            err.contains("Falcon signature") || err.contains("invalid"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn verify_lookup_signature_ecdsa_happy_path() {
        let secret = EcdsaSecretKey::new();
        let public = secret.public_key();
        let key_commitment = public.to_commitment();
        let timestamp = 1_700_000_000_000i64;

        let digest = LookupAuthMessage::new(timestamp, key_commitment).to_word();
        let signature = secret.sign(digest);
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        let parsed = parse_lookup_pubkey(&ecdsa_pubkey_hex(&secret)).expect("parse ECDSA");
        verify_lookup_signature(&parsed, timestamp, key_commitment, &signature_hex)
            .expect("valid signature must verify");
    }

    #[test]
    fn verify_lookup_signature_ecdsa_rejects_wrong_commitment() {
        let secret = EcdsaSecretKey::new();
        let public = secret.public_key();
        let real_commitment = public.to_commitment();
        let timestamp = 1_700_000_000_000i64;

        let digest = LookupAuthMessage::new(timestamp, real_commitment).to_word();
        let signature = secret.sign(digest);
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        let parsed = parse_lookup_pubkey(&ecdsa_pubkey_hex(&secret)).expect("parse ECDSA");

        // Now ask for verification under a different key_commitment than was
        // signed. The digest the verifier reconstructs differs, so verification
        // fails — the verifier must not silently substitute the signed value.
        let wrong_commitment = Word::from([1u32, 1, 1, 1]);
        let err = verify_lookup_signature(&parsed, timestamp, wrong_commitment, &signature_hex)
            .expect_err("wrong commitment must fail verification");
        assert!(err.contains("ECDSA signature verification failed"), "{err}");
    }

    #[test]
    fn verify_lookup_signature_rejects_signature_for_auth_request_message() {
        // Cross-domain replay test: a signature crafted under
        // AuthRequestMessage::to_word() must NOT validate when interpreted as
        // a LookupAuthMessage signature, even when timestamp and the 4-felt
        // payload align with the queried key_commitment.
        let secret = FalconSecretKey::new();
        let public = secret.public_key();
        let key_commitment = public.to_commitment();
        let timestamp = 1_700_000_000_000i64;

        // Build an AuthRequestMessage digest using the commitment as its payload.
        let account_id =
            AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").expect("account id");
        let payload = AuthRequestPayload::from_bytes(&key_commitment.as_bytes());
        let request_digest = AuthRequestMessage::new(account_id, timestamp, payload).to_word();

        // Sign the AuthRequestMessage digest, then attempt to use that signature
        // against the lookup verifier, which reconstructs a LookupAuthMessage
        // digest. They are domain-separated, so verification must fail.
        let signature = secret.sign(request_digest);
        let signature_hex = signature.into_hex();

        let parsed = parse_lookup_pubkey(&falcon_pubkey_hex(&secret)).expect("parse Falcon");
        let err = verify_lookup_signature(&parsed, timestamp, key_commitment, &signature_hex)
            .expect_err("cross-domain signature must be rejected");
        assert!(err.contains("Falcon signature"), "{err}");
    }
}
