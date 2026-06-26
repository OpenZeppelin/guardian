use std::fmt;
use std::sync::Arc;

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use serde_json::Value;
use zeroize::Zeroizing;

use super::envelope::{ALG_AES_256_GCM, ENVELOPE_VERSION, Envelope, RecordAad};
use super::key_provider::{KeyProviderError, StorageKeyProvider};

const NONCE_LEN: usize = 12;

#[derive(Debug)]
pub(crate) enum CipherError {
    NotAnEnvelope,
    UnsupportedVersion(u8),
    UnsupportedAlgorithm(String),
    InvalidNonce,
    DecryptionFailed,
    EncryptionFailed,
    KeyProvider(KeyProviderError),
}

impl fmt::Display for CipherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CipherError::NotAnEnvelope => write!(f, "stored payload is not an encryption envelope"),
            CipherError::UnsupportedVersion(v) => write!(f, "unsupported envelope version {v}"),
            CipherError::UnsupportedAlgorithm(alg) => {
                write!(f, "unsupported envelope algorithm '{alg}'")
            }
            CipherError::InvalidNonce => write!(f, "envelope nonce is malformed"),
            CipherError::DecryptionFailed => write!(f, "decryption failed"),
            CipherError::EncryptionFailed => write!(f, "encryption failed"),
            CipherError::KeyProvider(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for CipherError {}

/// Authenticated encryption of storage payloads, binding each ciphertext to its
/// record identity via AEAD additional authenticated data.
pub(crate) trait StorageCipher: Send + Sync {
    fn encrypt(&self, aad: &RecordAad, plaintext: &Value) -> Result<Value, CipherError>;
    fn decrypt(&self, aad: &RecordAad, envelope: &Value) -> Result<Value, CipherError>;
}

pub(crate) struct Aes256GcmCipher {
    keys: Arc<dyn StorageKeyProvider>,
}

impl Aes256GcmCipher {
    pub(crate) fn new(keys: Arc<dyn StorageKeyProvider>) -> Self {
        Self { keys }
    }

    fn cipher_for(&self, kid: &str) -> Result<Aes256Gcm, CipherError> {
        let key = self.keys.key(kid).map_err(CipherError::KeyProvider)?;
        Ok(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(
            key.expose_secret(),
        )))
    }
}

impl StorageCipher for Aes256GcmCipher {
    fn encrypt(&self, aad: &RecordAad, plaintext: &Value) -> Result<Value, CipherError> {
        let kid = self.keys.active_key_id().to_string();
        let cipher = self.cipher_for(&kid)?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

        let plaintext_bytes = Zeroizing::new(
            serde_json::to_vec(plaintext).map_err(|_| CipherError::EncryptionFailed)?,
        );
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: &plaintext_bytes,
                    aad: &aad.to_bytes(),
                },
            )
            .map_err(|_| CipherError::EncryptionFailed)?;

        let envelope = Envelope {
            v: ENVELOPE_VERSION,
            alg: ALG_AES_256_GCM.to_string(),
            kid,
            nonce: BASE64.encode(nonce),
            ct: BASE64.encode(ciphertext),
        };
        serde_json::to_value(envelope).map_err(|_| CipherError::EncryptionFailed)
    }

    fn decrypt(&self, aad: &RecordAad, envelope: &Value) -> Result<Value, CipherError> {
        let envelope = Envelope::deserialize(envelope).map_err(|_| CipherError::NotAnEnvelope)?;
        if envelope.v != ENVELOPE_VERSION {
            return Err(CipherError::UnsupportedVersion(envelope.v));
        }
        if envelope.alg != ALG_AES_256_GCM {
            return Err(CipherError::UnsupportedAlgorithm(envelope.alg));
        }

        let cipher = self.cipher_for(&envelope.kid)?;
        let nonce_bytes = BASE64
            .decode(&envelope.nonce)
            .map_err(|_| CipherError::InvalidNonce)?;
        if nonce_bytes.len() != NONCE_LEN {
            return Err(CipherError::InvalidNonce);
        }
        let ciphertext = BASE64
            .decode(&envelope.ct)
            .map_err(|_| CipherError::DecryptionFailed)?;

        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    Nonce::from_slice(&nonce_bytes),
                    Payload {
                        msg: &ciphertext,
                        aad: &aad.to_bytes(),
                    },
                )
                .map_err(|_| CipherError::DecryptionFailed)?,
        );
        serde_json::from_slice(&plaintext).map_err(|_| CipherError::DecryptionFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::encryption::key_provider::InMemoryKeyProvider;
    use serde_json::json;

    fn cipher_with(byte: u8, kid: &str) -> Aes256GcmCipher {
        let key_b64 = BASE64.encode([byte; 32]);
        let provider = InMemoryKeyProvider::from_dev_key(&key_b64, kid).unwrap();
        Aes256GcmCipher::new(Arc::new(provider))
    }

    fn sample() -> Value {
        json!({ "balance": 42, "owner": "alice", "nested": { "k": [1, 2, 3] } })
    }

    #[test]
    fn roundtrip_returns_original() {
        let cipher = cipher_with(1, "k1");
        let aad = RecordAad::State {
            account_id: "acct1",
        };
        let env = cipher.encrypt(&aad, &sample()).unwrap();
        assert_eq!(cipher.decrypt(&aad, &env).unwrap(), sample());
    }

    #[test]
    fn nonce_and_ciphertext_are_fresh_per_encryption() {
        let cipher = cipher_with(1, "k1");
        let aad = RecordAad::Delta {
            account_id: "acct1",
            nonce: 5,
        };
        let a: Envelope = serde_json::from_value(cipher.encrypt(&aad, &sample()).unwrap()).unwrap();
        let b: Envelope = serde_json::from_value(cipher.encrypt(&aad, &sample()).unwrap()).unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ct, b.ct);
        assert_eq!(BASE64.decode(&a.nonce).unwrap().len(), NONCE_LEN);
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let cipher = cipher_with(1, "k1");
        let aad = RecordAad::State {
            account_id: "acct1",
        };
        let mut env: Envelope =
            serde_json::from_value(cipher.encrypt(&aad, &sample()).unwrap()).unwrap();
        let mut raw = BASE64.decode(&env.ct).unwrap();
        raw[0] ^= 0xff;
        env.ct = BASE64.encode(raw);
        let err = cipher
            .decrypt(&aad, &serde_json::to_value(env).unwrap())
            .unwrap_err();
        assert!(matches!(err, CipherError::DecryptionFailed));
    }

    #[test]
    fn wrong_key_is_rejected() {
        let writer = cipher_with(1, "k1");
        let reader = cipher_with(2, "k1");
        let aad = RecordAad::State {
            account_id: "acct1",
        };
        let env = writer.encrypt(&aad, &sample()).unwrap();
        assert!(matches!(
            reader.decrypt(&aad, &env).unwrap_err(),
            CipherError::DecryptionFailed
        ));
    }

    #[test]
    fn aad_mismatch_is_rejected() {
        let cipher = cipher_with(1, "k1");
        let env = cipher
            .encrypt(
                &RecordAad::State {
                    account_id: "acct1",
                },
                &sample(),
            )
            .unwrap();
        let err = cipher
            .decrypt(
                &RecordAad::State {
                    account_id: "acct2",
                },
                &env,
            )
            .unwrap_err();
        assert!(matches!(err, CipherError::DecryptionFailed));
    }

    #[test]
    fn proposal_cannot_be_decrypted_under_a_different_commitment() {
        let cipher = cipher_with(1, "k1");
        let env = cipher
            .encrypt(
                &RecordAad::Proposal {
                    account_id: "acct1",
                    commitment: "0xaaa",
                },
                &sample(),
            )
            .unwrap();
        let err = cipher
            .decrypt(
                &RecordAad::Proposal {
                    account_id: "acct1",
                    commitment: "0xbbb",
                },
                &env,
            )
            .unwrap_err();
        assert!(matches!(err, CipherError::DecryptionFailed));
    }

    #[test]
    fn unknown_kid_is_rejected() {
        let cipher = cipher_with(1, "k1");
        let aad = RecordAad::State {
            account_id: "acct1",
        };
        let mut env: Envelope =
            serde_json::from_value(cipher.encrypt(&aad, &sample()).unwrap()).unwrap();
        env.kid = "k9".to_string();
        let err = cipher
            .decrypt(&aad, &serde_json::to_value(env).unwrap())
            .unwrap_err();
        assert!(matches!(
            err,
            CipherError::KeyProvider(KeyProviderError::UnknownKeyId(_))
        ));
    }

    #[test]
    fn plaintext_value_is_not_an_envelope() {
        let cipher = cipher_with(1, "k1");
        let err = cipher
            .decrypt(
                &RecordAad::State {
                    account_id: "acct1",
                },
                &sample(),
            )
            .unwrap_err();
        assert!(matches!(err, CipherError::NotAnEnvelope));
    }

    #[ignore = "timing benchmark; run on demand with --ignored"]
    #[test]
    fn cipher_path_latency_is_sub_millisecond() {
        let cipher = cipher_with(1, "k1");
        let aad = RecordAad::State {
            account_id: "acct1",
        };
        let payload = json!({ "blob": "x".repeat(8 * 1024) });

        let iterations = 1_000u32;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let env = cipher.encrypt(&aad, &payload).unwrap();
            let _ = cipher.decrypt(&aad, &env).unwrap();
        }
        let per_op = start.elapsed() / iterations;
        println!("encrypt+decrypt per op: {per_op:?}");
        if !cfg!(debug_assertions) {
            assert!(
                per_op < std::time::Duration::from_millis(1),
                "cipher path exceeded 1ms/op in release: {per_op:?}"
            );
        }
    }

    #[test]
    fn rotated_provider_decrypts_old_and_new_records() {
        let aad = RecordAad::State {
            account_id: "acct1",
        };

        let k1_only = Aes256GcmCipher::new(Arc::new(
            InMemoryKeyProvider::from_dev_key(&BASE64.encode([1u8; 32]), "k1").unwrap(),
        ));
        let old_record = k1_only.encrypt(&aad, &sample()).unwrap();

        let secret = format!(
            r#"{{"active":"k2","keys":{{"k1":"{}","k2":"{}"}}}}"#,
            BASE64.encode([1u8; 32]),
            BASE64.encode([2u8; 32])
        );
        let rotated = Aes256GcmCipher::new(Arc::new(
            InMemoryKeyProvider::from_secret_json(&secret).unwrap(),
        ));

        assert_eq!(rotated.decrypt(&aad, &old_record).unwrap(), sample());

        let new_record = rotated.encrypt(&aad, &sample()).unwrap();
        let stamped: Envelope = serde_json::from_value(new_record.clone()).unwrap();
        assert_eq!(stamped.kid, "k2");
        assert_eq!(rotated.decrypt(&aad, &new_record).unwrap(), sample());
    }
}
