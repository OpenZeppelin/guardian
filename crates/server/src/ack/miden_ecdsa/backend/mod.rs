//! Pluggable backends for the ECDSA ACK signer.
//!
//! [`EcdsaSignerBackend`] is the runtime seam: it owns the public key and
//! produces a Miden `ecdsa_k256_keccak` [`Signature`] for a message, whether
//! the private key lives in process (in-memory) or in a hosted service (AWS
//! KMS). A new provider is added by implementing the trait and wiring a
//! variant; the ACK flow never changes. [`EcdsaBackendKind`] is the static
//! startup selector, not the extensibility boundary.

mod aws_kms;
mod in_memory;

pub(crate) use aws_kms::AwsKmsEcdsaBackend;
pub(crate) use in_memory::InMemoryEcdsaBackend;

use crate::error::{GuardianError, Result};
use async_trait::async_trait;
use miden_protocol::Word;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature};

const ENV_ECDSA_BACKEND: &str = "GUARDIAN_ACK_ECDSA_BACKEND";
const BACKEND_IN_MEMORY: &str = "in-memory";
const BACKEND_AWS_KMS: &str = "aws-kms";

#[async_trait]
pub(crate) trait EcdsaSignerBackend: Send + Sync {
    fn public_key(&self) -> &PublicKey;
    async fn sign(&self, message: Word) -> Result<Signature>;
    fn backend_id(&self) -> &'static str;
}

/// Selects which ECDSA backend to construct at startup. New providers are added
/// by implementing [`EcdsaSignerBackend`] (the runtime seam) and adding a variant
/// here plus an arm in `build_ecdsa_signer`; the enum is the static selection
/// point, not the extensibility boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EcdsaBackendKind {
    InMemory,
    AwsKms,
}

impl EcdsaBackendKind {
    pub(crate) fn from_env() -> Result<Self> {
        match std::env::var(ENV_ECDSA_BACKEND) {
            Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
                "" | BACKEND_IN_MEMORY => Ok(Self::InMemory),
                BACKEND_AWS_KMS => Ok(Self::AwsKms),
                other => Err(GuardianError::ConfigurationError(format!(
                    "{ENV_ECDSA_BACKEND} `{other}` is not supported (expected `{BACKEND_IN_MEMORY}` or `{BACKEND_AWS_KMS}`)"
                ))),
            },
            Err(std::env::VarError::NotPresent) => Ok(Self::InMemory),
            Err(std::env::VarError::NotUnicode(_)) => Err(GuardianError::ConfigurationError(
                format!("{ENV_ECDSA_BACKEND} must contain valid UTF-8"),
            )),
        }
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(value: Option<&str>) -> Self {
            let previous = std::env::var(ENV_ECDSA_BACKEND).ok();
            match value {
                Some(value) => unsafe { std::env::set_var(ENV_ECDSA_BACKEND, value) },
                None => unsafe { std::env::remove_var(ENV_ECDSA_BACKEND) },
            }
            Self { previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(ENV_ECDSA_BACKEND, value) },
                None => unsafe { std::env::remove_var(ENV_ECDSA_BACKEND) },
            }
        }
    }

    #[test]
    fn defaults_to_in_memory_when_unset() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvVarGuard::set(None);
        assert_eq!(
            EcdsaBackendKind::from_env().unwrap(),
            EcdsaBackendKind::InMemory
        );
    }

    #[test]
    fn parses_aws_kms_case_insensitively() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvVarGuard::set(Some("AWS-KMS"));
        assert_eq!(
            EcdsaBackendKind::from_env().unwrap(),
            EcdsaBackendKind::AwsKms
        );
    }

    #[test]
    fn unknown_backend_fails_listing_supported_ids() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvVarGuard::set(Some("vault"));
        let error = EcdsaBackendKind::from_env().unwrap_err();
        match error {
            GuardianError::ConfigurationError(message) => {
                assert!(message.contains("vault"));
                assert!(message.contains(BACKEND_IN_MEMORY));
                assert!(message.contains(BACKEND_AWS_KMS));
            }
            other => panic!("expected ConfigurationError, got {other:?}"),
        }
    }

    struct StubBackend {
        secret: miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey,
        public_key: PublicKey,
    }

    impl StubBackend {
        fn new() -> Self {
            let secret = miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey::new();
            let public_key = secret.public_key();
            Self { secret, public_key }
        }
    }

    #[async_trait::async_trait]
    impl EcdsaSignerBackend for StubBackend {
        fn public_key(&self) -> &PublicKey {
            &self.public_key
        }

        async fn sign(&self, message: Word) -> Result<Signature> {
            Ok(self.secret.sign(message))
        }

        fn backend_id(&self) -> &'static str {
            "stub"
        }
    }

    #[tokio::test]
    async fn custom_backend_plugs_in_through_trait_only() {
        use crate::ack::MidenEcdsaSigner;
        use miden_keystore::ecdsa_commitment_hex;
        use std::sync::Arc;

        let stub = StubBackend::new();
        let expected = ecdsa_commitment_hex(stub.public_key());

        let signer = MidenEcdsaSigner::new(Arc::new(stub));

        assert_eq!(signer.commitment_hex(), expected);
    }

    #[tokio::test]
    async fn ack_delta_signature_round_trips_and_verifies() {
        use crate::ack::MidenEcdsaSigner;
        use crate::delta_object::{DeltaObject, DeltaStatus};
        use crate::testing::helpers::create_test_delta_payload;
        use guardian_shared::FromJson;
        use miden_protocol::transaction::TransactionSummary;
        use miden_protocol::utils::serde::Deserializable;
        use std::sync::Arc;

        let stub = StubBackend::new();
        let public_key = stub.public_key().clone();
        let signer = MidenEcdsaSigner::new(Arc::new(stub));

        let account_id = "0x7bfb0f38b0fafa103f86a805594170";
        let delta = DeltaObject {
            account_id: account_id.to_string(),
            nonce: 1,
            prev_commitment: "0xprev".to_string(),
            new_commitment: None,
            delta_payload: create_test_delta_payload(account_id),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Pending {
                timestamp: "2026-06-03T00:00:00Z".to_string(),
                proposer_id: "0xproposer".to_string(),
                cosigner_sigs: vec![],
            },
            metadata: None,
        };

        let acked = signer.ack_delta(delta).await.unwrap();

        assert!(!acked.ack_sig.is_empty());
        let signature = Signature::read_from_bytes(&hex::decode(&acked.ack_sig).unwrap()).unwrap();
        let tx_commitment = TransactionSummary::from_json(&acked.delta_payload)
            .unwrap()
            .to_commitment();
        assert!(public_key.verify(tx_commitment, &signature));
    }
}
