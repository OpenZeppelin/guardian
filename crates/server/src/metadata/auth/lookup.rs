//! Authentication helpers for the `/state/lookup` endpoint.
//!
//! Raw lookup derives the public key from the signature (Falcon embeds it;
//! ECDSA recovers it). EIP-712 lookup verifies against `x-pubkey`. Both paths
//! require the verified key's commitment to match the queried commitment.

use guardian_shared::auth_request_eip712::lookup_digest;
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

/// Serialized ECDSA secp256k1 signature length: `r (32) || s (32) || v (1)`.
const ECDSA_SIGNATURE_LEN: usize = 65;

/// A public key derived from a lookup signature.
#[derive(Debug)]
pub enum LookupPublicKey {
    Falcon(FalconPublicKey),
    Ecdsa(EcdsaPublicKey),
}

/// Derive the `0x`-prefixed lowercase hex commitment of a `LookupPublicKey`.
/// Result is comparable directly to `cosigner_commitments` entries.
pub fn commitment_of(pk: &LookupPublicKey) -> String {
    let commitment: Word = match pk {
        LookupPublicKey::Falcon(key) => key.to_commitment(),
        LookupPublicKey::Ecdsa(key) => key.to_commitment(),
    };
    format!("0x{}", hex::encode(commitment.as_bytes()))
}

/// Derive the public key from a lookup signature and verify the signature
/// cryptographically against the lookup digest, all in one step.
///
/// Falcon signatures embed the public key; ECDSA signatures recover it via
/// the recovery byte. Try Falcon first (exactly 1524 bytes, enforced by
/// `FromHex`); otherwise take the ECDSA path only for exactly 65 bytes
/// (`r || s || v`), because `read_from_bytes` ignores trailing bytes and would
/// read the prefix of an unparseable Falcon blob as ECDSA. If the bytes
/// neither parse as a Falcon signature nor recover an ECDSA pubkey, the
/// request is rejected.
///
/// Caller is still responsible for the commitment-equality check
/// (`commitment_of(returned_pk) == queried_key_commitment`) — that's where
/// proof-of-possession against the queried commitment is enforced.
pub fn derive_pubkey_from_lookup_signature(
    signature_hex: &str,
    timestamp_ms: i64,
    key_commitment: Word,
) -> Result<LookupPublicKey, String> {
    let digest = LookupAuthMessage::new(timestamp_ms, key_commitment).to_word();
    derive_pubkey_from_raw_signature(signature_hex, digest)
}

/// Derive the public key from a raw (Falcon or ECDSA) signature over `digest`
/// and verify it in one step; see [`derive_pubkey_from_lookup_signature`] for
/// the parsing rules. Shared by the lookup and session revoke-all paths.
pub fn derive_pubkey_from_raw_signature(
    signature_hex: &str,
    digest: Word,
) -> Result<LookupPublicKey, String> {
    if let Ok(signature) = FalconSignature::from_hex(signature_hex) {
        let public_key = signature.public_key().clone();
        if public_key.verify(digest, &signature) {
            return Ok(LookupPublicKey::Falcon(public_key));
        }
        return Err("Falcon signature verification failed".to_string());
    }

    let bytes = hex::decode(signature_hex.trim_start_matches("0x"))
        .map_err(|e| format!("invalid signature hex: {e}"))?;
    if bytes.len() == ECDSA_SIGNATURE_LEN
        && let Ok(signature) = EcdsaSignature::read_from_bytes(&bytes)
    {
        let recovered = EcdsaPublicKey::recover_from(digest, &signature)
            .map_err(|_| "ECDSA signature recovery failed".to_string())?;
        if recovered.verify(digest, &signature) {
            return Ok(LookupPublicKey::Ecdsa(recovered));
        }
        return Err("ECDSA signature verification failed after recovery".to_string());
    }

    Err("signature did not parse as Falcon or ECDSA".to_string())
}

/// Verify a typed lookup signature against the public key supplied by the caller.
pub fn verify_eip712_lookup_signature(
    signature_hex: &str,
    public_key_hex: &str,
    timestamp_ms: i64,
    key_commitment: Word,
) -> Result<LookupPublicKey, String> {
    let lookup_hash = LookupAuthMessage::new(timestamp_ms, key_commitment).to_word();
    verify_eip712_signature(
        "lookup",
        signature_hex,
        public_key_hex,
        lookup_digest(lookup_hash),
    )
}

/// Verify an ECDSA signature over an EIP-712 `digest` against the supplied
/// public key. Shared by the lookup and session revoke-all paths; `label`
/// names the message in errors.
pub fn verify_eip712_signature(
    label: &str,
    signature_hex: &str,
    public_key_hex: &str,
    digest: [u8; 32],
) -> Result<LookupPublicKey, String> {
    let label = format!("EIP-712 {label}");
    let signature = parse_ecdsa_signature(&label, signature_hex)?;
    let public_key = parse_ecdsa_public_key(&label, public_key_hex)?;
    if !public_key.verify_prehash(digest, &signature) {
        return Err(format!("{label} signature verification failed"));
    }
    Ok(LookupPublicKey::Ecdsa(public_key))
}

/// Whether `signature_hex` has the length of a raw ECDSA signature.
pub fn is_ecdsa_signature_hex(signature_hex: &str) -> bool {
    hex::decode(signature_hex.trim_start_matches("0x"))
        .is_ok_and(|bytes| bytes.len() == ECDSA_SIGNATURE_LEN)
}

/// Verify a raw ECDSA signature over `digest` against the supplied public
/// key: the fallback for wallets whose signature does not recover to their
/// key (another recovery-byte encoding), as request auth already allows.
pub fn verify_raw_ecdsa_signature(
    label: &str,
    signature_hex: &str,
    public_key_hex: &str,
    digest: Word,
) -> Result<LookupPublicKey, String> {
    let signature = parse_ecdsa_signature(label, signature_hex)?;
    let public_key = parse_ecdsa_public_key(label, public_key_hex)?;
    if !public_key.verify(digest, &signature) {
        return Err(format!("{label} signature verification failed"));
    }
    Ok(LookupPublicKey::Ecdsa(public_key))
}

fn parse_ecdsa_signature(label: &str, signature_hex: &str) -> Result<EcdsaSignature, String> {
    let signature_bytes = hex::decode(signature_hex.trim_start_matches("0x"))
        .map_err(|e| format!("invalid {label} signature hex: {e}"))?;
    if signature_bytes.len() != ECDSA_SIGNATURE_LEN {
        return Err(format!(
            "invalid {label} signature: expected {ECDSA_SIGNATURE_LEN} bytes, got {}",
            signature_bytes.len()
        ));
    }
    EcdsaSignature::read_from_bytes(&signature_bytes)
        .map_err(|e| format!("invalid {label} signature: {e}"))
}

fn parse_ecdsa_public_key(label: &str, public_key_hex: &str) -> Result<EcdsaPublicKey, String> {
    let public_key_bytes = hex::decode(public_key_hex.trim_start_matches("0x"))
        .map_err(|e| format!("invalid {label} public key hex: {e}"))?;
    EcdsaPublicKey::read_from_bytes(&public_key_bytes)
        .map_err(|e| format!("invalid {label} public key: {e}"))
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use guardian_shared::auth_request_eip712::{lookup_digest, request_digest};
    use guardian_shared::auth_request_message::AuthRequestMessage;
    use guardian_shared::auth_request_payload::AuthRequestPayload;
    use guardian_shared::hex::IntoHex;
    use miden_protocol::Word;
    use miden_protocol::account::AccountId;
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey as EcdsaSecretKey;
    use miden_protocol::crypto::dsa::falcon512_poseidon2::{PK_LEN, SecretKey as FalconSecretKey};
    use miden_protocol::utils::serde::Serializable;

    #[test]
    fn eip712_lookup_binds_key_commitment_and_timestamp() {
        let signer = EcdsaSecretKey::new();
        let public_key = signer.public_key();
        let public_key_hex = format!("0x{}", hex::encode(public_key.to_bytes()));
        let commitment = public_key.to_commitment();
        let timestamp = 1_700_000_000_000;
        let lookup_hash = LookupAuthMessage::new(timestamp, commitment).to_word();
        let signature = signer.sign_prehash(lookup_digest(lookup_hash));
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        let verified =
            verify_eip712_lookup_signature(&signature_hex, &public_key_hex, timestamp, commitment)
                .unwrap();
        assert_eq!(commitment_of(&verified), commitment_hex_word(commitment));
        assert!(
            verify_eip712_lookup_signature(
                &signature_hex,
                &public_key_hex,
                timestamp + 1,
                commitment,
            )
            .is_err()
        );
        assert!(
            verify_eip712_lookup_signature(
                &signature_hex,
                &public_key_hex,
                timestamp,
                Word::default(),
            )
            .is_err()
        );
        let other_public_key = EcdsaSecretKey::new().public_key();
        let other_public_key_hex = format!("0x{}", hex::encode(other_public_key.to_bytes()));
        assert!(
            verify_eip712_lookup_signature(
                &signature_hex,
                &other_public_key_hex,
                timestamp,
                commitment,
            )
            .is_err()
        );
        let request_signature = signer.sign_prehash(request_digest(lookup_hash));
        assert!(
            verify_eip712_lookup_signature(
                &format!("0x{}", hex::encode(request_signature.to_bytes())),
                &public_key_hex,
                timestamp,
                commitment,
            )
            .is_err()
        );
        let mut unnormalized = signature.to_bytes();
        unnormalized[64] += 27;
        assert!(
            verify_eip712_lookup_signature(
                &format!("0x{}", hex::encode(unnormalized)),
                &public_key_hex,
                timestamp,
                commitment,
            )
            .is_err()
        );
    }

    #[test]
    fn eip712_lookup_rejects_wrong_length() {
        let signer = EcdsaSecretKey::new();
        let public_key = signer.public_key();
        let public_key_hex = format!("0x{}", hex::encode(public_key.to_bytes()));
        let commitment = public_key.to_commitment();
        let timestamp = 1_700_000_000_000;
        let lookup_hash = LookupAuthMessage::new(timestamp, commitment).to_word();
        let signature = signer.sign_prehash(lookup_digest(lookup_hash)).to_bytes();

        let mut with_trailing_byte = signature.clone();
        with_trailing_byte.push(0);
        let err = verify_eip712_lookup_signature(
            &format!("0x{}", hex::encode(with_trailing_byte)),
            &public_key_hex,
            timestamp,
            commitment,
        )
        .expect_err("EIP-712 signature with a trailing byte must be rejected");
        assert!(err.contains("expected 65 bytes, got 66"), "{err}");

        let err = verify_eip712_lookup_signature(
            &format!("0x{}", hex::encode(&signature[..64])),
            &public_key_hex,
            timestamp,
            commitment,
        )
        .expect_err("truncated EIP-712 signature must be rejected");
        assert!(err.contains("expected 65 bytes, got 64"), "{err}");
    }

    fn commitment_hex_word(word: Word) -> String {
        format!("0x{}", hex::encode(word.as_bytes()))
    }

    fn falcon_lookup_signature(secret: &FalconSecretKey, ts: i64, kc: Word) -> String {
        let digest = LookupAuthMessage::new(ts, kc).to_word();
        secret.sign(digest).into_hex()
    }

    fn ecdsa_lookup_signature(secret: &EcdsaSecretKey, ts: i64, kc: Word) -> String {
        let digest = LookupAuthMessage::new(ts, kc).to_word();
        format!("0x{}", hex::encode(secret.sign(digest).to_bytes()))
    }

    #[test]
    fn derive_pubkey_falcon_happy_path() {
        let secret = FalconSecretKey::new();
        let kc = secret.public_key().to_commitment();
        let ts = 1_700_000_000_000i64;
        let sig = falcon_lookup_signature(&secret, ts, kc);

        let pk = derive_pubkey_from_lookup_signature(&sig, ts, kc).expect("derive Falcon");
        assert!(matches!(pk, LookupPublicKey::Falcon(_)));
        assert_eq!(commitment_of(&pk), commitment_hex_word(kc));
    }

    #[test]
    fn derive_pubkey_ecdsa_happy_path() {
        let secret = EcdsaSecretKey::new();
        let kc = secret.public_key().to_commitment();
        let ts = 1_700_000_000_000i64;
        let sig = ecdsa_lookup_signature(&secret, ts, kc);

        let pk = derive_pubkey_from_lookup_signature(&sig, ts, kc).expect("derive ECDSA");
        assert!(matches!(pk, LookupPublicKey::Ecdsa(_)));
        assert_eq!(commitment_of(&pk), commitment_hex_word(kc));
    }

    #[test]
    fn derive_pubkey_falcon_rejects_tampered_signature() {
        let secret = FalconSecretKey::new();
        let kc = secret.public_key().to_commitment();
        let ts = 1_700_000_000_000i64;

        let digest = LookupAuthMessage::new(ts, kc).to_word();
        let mut bytes = secret.sign(digest).to_bytes();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x01;
        let tampered = format!("0x{}", hex::encode(&bytes));

        let err = derive_pubkey_from_lookup_signature(&tampered, ts, kc)
            .expect_err("tampered signature must be rejected");
        assert!(
            err.contains("Falcon") || err.contains("did not parse"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn derive_pubkey_unparseable_falcon_is_not_read_as_ecdsa() {
        // A Falcon blob whose embedded key fails to parse must not fall back
        // to ECDSA, even when its first 65 bytes form a valid ECDSA encoding.
        let secret = FalconSecretKey::new();
        let kc = secret.public_key().to_commitment();
        let ts = 1_700_000_000_000i64;

        let digest = LookupAuthMessage::new(ts, kc).to_word();
        let mut bytes = secret.sign(digest).to_bytes();
        // Keep the key's header byte; set every 14-bit coefficient to 0x3fff (>= q).
        let key_coefficients = bytes.len() - PK_LEN + 1;
        bytes[key_coefficients..].fill(0xff);
        // Byte 64 is the ECDSA recovery id; 0 makes the 65-byte prefix valid ECDSA.
        bytes[64] = 0;
        assert!(FalconSignature::read_from_bytes(&bytes).is_err());
        assert!(EcdsaSignature::read_from_bytes(&bytes).is_ok());

        let err =
            derive_pubkey_from_lookup_signature(&format!("0x{}", hex::encode(&bytes)), ts, kc)
                .expect_err("unparseable Falcon signature must be rejected");
        assert!(err.contains("did not parse"), "unexpected error: {err}");
    }

    #[test]
    fn derive_pubkey_ecdsa_rejects_wrong_length() {
        let secret = EcdsaSecretKey::new();
        let kc = secret.public_key().to_commitment();
        let ts = 1_700_000_000_000i64;
        let sig = ecdsa_lookup_signature(&secret, ts, kc);

        let with_trailing_byte = format!("{sig}00");
        let err = derive_pubkey_from_lookup_signature(&with_trailing_byte, ts, kc)
            .expect_err("ECDSA signature with a trailing byte must be rejected");
        assert!(err.contains("did not parse"), "unexpected error: {err}");

        let truncated = &sig[..sig.len() - 2];
        let err = derive_pubkey_from_lookup_signature(truncated, ts, kc)
            .expect_err("truncated ECDSA signature must be rejected");
        assert!(err.contains("did not parse"), "unexpected error: {err}");
    }

    #[test]
    fn derive_pubkey_ecdsa_rejects_wrong_digest() {
        let secret = EcdsaSecretKey::new();
        let kc = secret.public_key().to_commitment();
        let ts = 1_700_000_000_000i64;
        let sig = ecdsa_lookup_signature(&secret, ts, kc);

        // ECDSA recovery is mathematically defined for any (digest, signature)
        // pair: passing a wrong digest does NOT make recovery fail, it
        // recovers a *different* public key (the one a hypothetical signer
        // would need to hold to have produced this signature over this
        // alternate digest). So this function may return Ok here — that's
        // fine, because the service-layer check
        // `commitment_of(returned_pk) == queried_key_commitment` is what
        // actually enforces proof-of-possession against the queried
        // commitment. This test asserts that downstream invariant: any
        // recovered key under a wrong digest cannot share the real signer's
        // commitment.
        let wrong_kc = Word::from([1u32, 1, 1, 1]);
        if let Ok(pk) = derive_pubkey_from_lookup_signature(&sig, ts, wrong_kc) {
            assert_ne!(
                commitment_of(&pk),
                commitment_hex_word(kc),
                "recovery under a wrong digest must never yield the real signer"
            );
        }
    }

    #[test]
    fn derive_pubkey_rejects_signature_for_auth_request_message() {
        // Cross-domain replay test: a signature crafted under
        // AuthRequestMessage::to_word() must NOT validate when interpreted
        // as a LookupAuthMessage signature.
        let secret = FalconSecretKey::new();
        let public = secret.public_key();
        let kc = public.to_commitment();
        let ts = 1_700_000_000_000i64;

        let account_id =
            AccountId::from_hex("0x8a8a8a8a8a8a8a010a8a8a8a8a8a8a").expect("account id");
        let payload = AuthRequestPayload::from_bytes(&kc.as_bytes());
        let request_digest = AuthRequestMessage::new(account_id, ts, payload).to_word();
        let sig = secret.sign(request_digest).into_hex();

        let err = derive_pubkey_from_lookup_signature(&sig, ts, kc)
            .expect_err("cross-domain signature must be rejected");
        assert!(err.contains("Falcon"), "{err}");
    }

    #[test]
    fn derive_pubkey_rejects_garbage_bytes() {
        let kc = Word::from([1u32, 1, 1, 1]);
        let err = derive_pubkey_from_lookup_signature("0xZZ", 1, kc)
            .expect_err("invalid hex must be rejected");
        assert!(err.contains("invalid signature hex"), "{err}");

        // Random short bytes that aren't a Falcon or ECDSA signature.
        let err = derive_pubkey_from_lookup_signature(&("0x".to_string() + &"ff".repeat(8)), 1, kc)
            .expect_err("garbage bytes must be rejected");
        assert!(
            err.contains("did not parse") || err.contains("recovery"),
            "unexpected error: {err}"
        );
    }
}
