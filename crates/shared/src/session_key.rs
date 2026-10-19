//! P-256 session keys.
//!
//! A session key signs the 32 bytes of a Guardian message word with ECDSA over
//! P-256 and SHA-256, which is what WebCrypto's
//! `sign({ name: "ECDSA", hash: "SHA-256" }, key, bytes)` produces for a
//! non-extractable browser key. Signatures are the 64-byte `r || s` encoding.

use miden_protocol::Word;
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};

use crate::session_grant::SESSION_PUBLIC_KEY_LEN;

/// Length of an `r || s` P-256 signature.
pub const SESSION_SIGNATURE_LEN: usize = 64;

/// Verifies a session-key signature over `message`.
pub fn verify(public_key: &[u8], message: Word, signature: &[u8]) -> Result<(), String> {
    let verifying_key = VerifyingKey::from_sec1_bytes(public_key)
        .map_err(|_| "Invalid session public key".to_string())?;
    let signature = Signature::from_slice(signature)
        .map_err(|_| format!("Session signature must be {SESSION_SIGNATURE_LEN} bytes (r || s)"))?;
    verifying_key
        .verify(&message.as_bytes(), &signature)
        .map_err(|_| "Session signature verification failed".to_string())
}

/// In-memory P-256 session key for Rust clients and tests.
#[derive(Clone)]
pub struct SessionKey {
    signing_key: SigningKey,
}

impl SessionKey {
    /// Builds a key from a 32-byte big-endian scalar.
    pub fn from_bytes(secret: &[u8; 32]) -> Result<Self, String> {
        SigningKey::from_slice(secret)
            .map(|signing_key| Self { signing_key })
            .map_err(|_| "Invalid P-256 session secret".to_string())
    }

    /// Generates a fresh random key.
    pub fn generate() -> Self {
        loop {
            let secret: [u8; 32] = rand::random();
            if let Ok(key) = Self::from_bytes(&secret) {
                return key;
            }
        }
    }

    /// SEC1-compressed public key.
    pub fn public_key(&self) -> [u8; SESSION_PUBLIC_KEY_LEN] {
        let encoded = self.signing_key.verifying_key().to_encoded_point(true);
        encoded
            .as_bytes()
            .try_into()
            .expect("compressed P-256 point is 33 bytes")
    }

    pub fn public_key_hex(&self) -> String {
        format!("0x{}", hex::encode(self.public_key()))
    }

    /// Signs the 32 bytes of `message`.
    pub fn sign(&self, message: Word) -> [u8; SESSION_SIGNATURE_LEN] {
        let signature: Signature = self.signing_key.sign(&message.as_bytes());
        signature.to_bytes().into()
    }

    pub fn sign_hex(&self, message: Word) -> String {
        format!("0x{}", hex::encode(self.sign(message)))
    }
}

impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionKey")
            .field("public_key", &self.public_key_hex())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SessionKey {
        SessionKey::from_bytes(&[seed; 32]).expect("valid scalar")
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let key = key(7);
        let message = Word::from([1u32, 2, 3, 4]);
        let signature = key.sign(message);

        assert!(verify(&key.public_key(), message, &signature).is_ok());
    }

    #[test]
    fn rejects_other_message_key_and_malformed_input() {
        let key = key(7);
        let message = Word::from([1u32, 2, 3, 4]);
        let signature = key.sign(message);

        assert!(verify(&key.public_key(), Word::from([1u32, 2, 3, 5]), &signature).is_err());
        assert!(verify(&self::key(8).public_key(), message, &signature).is_err());
        assert!(verify(&key.public_key(), message, &signature[..63]).is_err());
        assert!(verify(&[0x02; SESSION_PUBLIC_KEY_LEN], message, &signature).is_err());
    }

    #[test]
    fn accepts_high_s_signatures_from_webcrypto() {
        // WebCrypto does not normalize `s`, so the verifier must accept both
        // halves of the curve order.
        let key = key(9);
        let message = Word::from([9u32, 8, 7, 6]);
        let signature = Signature::from_slice(&key.sign(message)).unwrap();
        let flipped = match signature.normalize_s() {
            Some(low) if low != signature => signature,
            _ => {
                let (r, s) = signature.split_scalars();
                Signature::from_scalars(r, -*s).unwrap()
            }
        };

        assert!(verify(&key.public_key(), message, &flipped.to_bytes()).is_ok());
    }

    #[test]
    fn verifies_signature_produced_by_webcrypto() {
        // Generated in Node 24 with `crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" })`
        // on a fresh P-256 key, the same call a browser session signer makes.
        let public_key =
            hex::decode("024e1342217dee12b182871f6fe01d1b811aed14edd01596c68a087d5e2f2d4889")
                .unwrap();
        let signature = hex::decode(
            "2262f5570479f7c387c73e88ab1aa5c2ea18fdbc928caa359bdf13089d004ae7\
             0d9e7d2e7eb867b7bd054a6d3b9cdb71050f41bba469034280370346af960099",
        )
        .unwrap();
        let message = Word::from([1u32, 2, 3, 4]);

        assert!(verify(&public_key, message, &signature).is_ok());
        assert!(verify(&public_key, Word::from([1u32, 2, 3, 5]), &signature).is_err());
    }

    #[test]
    fn public_key_is_compressed_sec1() {
        let public_key = key(3).public_key();
        assert!(public_key[0] == 0x02 || public_key[0] == 0x03);
    }
}
