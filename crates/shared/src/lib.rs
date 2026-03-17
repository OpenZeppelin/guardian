use base64::Engine;
use miden_protocol::account::Account;
use miden_protocol::account::auth::Signature as AccountSignature;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak;
use miden_protocol::crypto::dsa::falcon512_rpo::Signature as FalconSignature;
use miden_protocol::transaction::TransactionSummary;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_protocol::{Felt, Hasher, Word};
use serde::{Deserialize, Serialize};

pub mod auth;
pub mod auth_request_message;
pub mod auth_request_payload;
pub mod hex;

use crate::hex::FromHex;

/// Supported signature schemes
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignatureScheme {
    Falcon,
    Ecdsa,
}

impl SignatureScheme {
    pub fn from(ack_scheme: &str) -> Result<Self, String> {
        match ack_scheme {
            value if value.eq_ignore_ascii_case("falcon") => Ok(Self::Falcon),
            value if value.eq_ignore_ascii_case("ecdsa") => Ok(Self::Ecdsa),
            value => Err(format!("unsupported signature scheme: {}", value)),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Falcon => "falcon",
            Self::Ecdsa => "ecdsa",
        }
    }

    pub fn parse_signature_hex(self, signature_hex: &str) -> Result<AccountSignature, String> {
        match self {
            Self::Falcon => {
                let signature = FalconSignature::from_hex(&ensure_hex_prefix(signature_hex))
                    .map_err(|e| format!("failed to parse Falcon signature: {}", e))?;
                Ok(AccountSignature::from(signature))
            }
            Self::Ecdsa => {
                let signature_bytes = ::hex::decode(signature_hex.trim_start_matches("0x"))
                    .map_err(|e| format!("invalid ECDSA signature hex: {}", e))?;
                let signature = ecdsa_k256_keccak::Signature::read_from_bytes(&signature_bytes)
                    .map_err(|e| format!("failed to parse ECDSA signature: {}", e))?;
                Ok(AccountSignature::EcdsaK256Keccak(signature))
            }
        }
    }

    pub fn build_signature_advice_entry(
        self,
        pubkey_commitment: Word,
        message: Word,
        signature: &AccountSignature,
        public_key_hex: Option<&str>,
    ) -> Result<(Word, Vec<Felt>), String> {
        let key = signature_advice_key(pubkey_commitment, message);

        let values = match (self, signature) {
            (Self::Falcon, AccountSignature::Falcon512Rpo(_)) => {
                signature.to_prepared_signature(message)
            }
            (Self::Falcon, _) => {
                return Err("expected Falcon signature for falcon scheme".to_string());
            }
            (Self::Ecdsa, AccountSignature::EcdsaK256Keccak(ecdsa_signature)) => {
                let public_key_hex = public_key_hex.ok_or_else(|| {
                    "ECDSA signature requires public key for advice preparation".to_string()
                })?;
                let public_key = parse_ecdsa_public_key_hex(public_key_hex)?;
                let mut encoded = encode_ecdsa_signature_felts(&public_key, ecdsa_signature);
                encoded.reverse();
                encoded
            }
            (Self::Ecdsa, _) => {
                return Err("expected ECDSA signature for ecdsa scheme".to_string());
            }
        };

        Ok((key, values))
    }
}

impl std::fmt::Display for SignatureScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

fn ensure_hex_prefix(hex: &str) -> String {
    if hex.starts_with("0x") {
        hex.to_string()
    } else {
        format!("0x{}", hex)
    }
}

fn signature_advice_key(pubkey_commitment: Word, message: Word) -> Word {
    let mut elements = Vec::with_capacity(8);
    elements.extend_from_slice(pubkey_commitment.as_elements());
    elements.extend_from_slice(message.as_elements());
    Hasher::hash_elements(&elements)
}

fn parse_ecdsa_public_key_hex(
    public_key_hex: &str,
) -> Result<ecdsa_k256_keccak::PublicKey, String> {
    let public_key_bytes = ::hex::decode(public_key_hex.trim_start_matches("0x"))
        .map_err(|e| format!("invalid ECDSA public key hex: {}", e))?;
    ecdsa_k256_keccak::PublicKey::read_from_bytes(&public_key_bytes)
        .map_err(|e| format!("failed to deserialize ECDSA public key: {}", e))
}

fn bytes_to_packed_u32_felts(bytes: &[u8]) -> Vec<Felt> {
    bytes
        .chunks(4)
        .map(|chunk| {
            let mut packed = [0u8; 4];
            packed[..chunk.len()].copy_from_slice(chunk);
            Felt::from(u32::from_le_bytes(packed))
        })
        .collect()
}

fn encode_ecdsa_signature_felts(
    public_key: &ecdsa_k256_keccak::PublicKey,
    signature: &ecdsa_k256_keccak::Signature,
) -> Vec<Felt> {
    let mut out = Vec::new();
    out.extend(bytes_to_packed_u32_felts(&public_key.to_bytes()));
    out.extend(bytes_to_packed_u32_felts(&signature.to_bytes()));
    out
}

/// Signature type for delta proposals
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "scheme", rename_all = "snake_case")]
pub enum ProposalSignature {
    Falcon {
        /// Hex-encoded Falcon signature
        signature: String,
    },
    Ecdsa {
        /// Hex-encoded ECDSA secp256k1 signature
        signature: String,
        /// Hex-encoded ECDSA public key (required for signature preparation)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        public_key: Option<String>,
    },
}

impl ProposalSignature {
    /// Creates a ProposalSignature from a scheme, hex-encoded signature, and optional public key.
    pub fn from_scheme(
        scheme: SignatureScheme,
        signature: String,
        public_key: Option<String>,
    ) -> Self {
        match scheme {
            SignatureScheme::Falcon => ProposalSignature::Falcon { signature },
            SignatureScheme::Ecdsa => ProposalSignature::Ecdsa {
                signature,
                public_key,
            },
        }
    }

    /// Returns the public key hex if this is an ECDSA signature with a public key.
    pub fn public_key(&self) -> Option<&str> {
        match self {
            ProposalSignature::Ecdsa { public_key, .. } => public_key.as_deref(),
            _ => None,
        }
    }
}

/// Delta payload structure containing transaction summary and signatures
/// This is the standard format for delta_payload in proposals
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeltaPayload {
    pub tx_summary: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<DeltaSignature>,
}

impl DeltaPayload {
    pub fn new(tx_summary: serde_json::Value) -> Self {
        Self {
            tx_summary,
            signatures: Vec::new(),
        }
    }

    pub fn with_signature(mut self, signature: DeltaSignature) -> Self {
        self.signatures.push(signature);
        self
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("DeltaPayload should always serialize")
    }
}

/// Signature entry in delta payload
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeltaSignature {
    pub signer_id: String,
    pub signature: ProposalSignature,
}

pub trait ToJson {
    fn to_json(&self) -> serde_json::Value;
}

pub trait FromJson: Sized {
    fn from_json(json: &serde_json::Value) -> Result<Self, String>;
}

impl ToJson for Account {
    fn to_json(&self) -> serde_json::Value {
        let bytes = self.to_bytes();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        serde_json::json!({
          "data": encoded,
          "account_id": self.id().to_hex(),
        })
    }
}

impl FromJson for Account {
    fn from_json(json: &serde_json::Value) -> Result<Self, String> {
        let encoded = json
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or("Missing or invalid 'data' field")?;

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| format!("Base64 decode error: {e}"))?;

        Account::read_from_bytes(&bytes).map_err(|e| format!("Deserialization error: {e}"))
    }
}

impl ToJson for TransactionSummary {
    fn to_json(&self) -> serde_json::Value {
        let bytes = self.to_bytes();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        serde_json::json!({
          "data": encoded,
        })
    }
}

impl FromJson for TransactionSummary {
    fn from_json(json: &serde_json::Value) -> Result<Self, String> {
        let encoded = json
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or("Missing or invalid 'data' field in delta payload")?;

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| format!("Base64 decode error: {e}"))?;

        TransactionSummary::read_from_bytes(&bytes)
            .map_err(|e| format!("AccountDelta deserialization error: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_protocol::{
        account::auth::Signature as AccountSignature,
        account::{AccountBuilder, auth::PublicKeyCommitment},
        crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey,
        crypto::dsa::falcon512_rpo::SecretKey,
    };
    use miden_standards::account::{auth::AuthFalcon512Rpo, wallets::BasicWallet};

    #[test]
    fn test_account_json_round_trip() {
        // Create a test account
        let secret_key = SecretKey::new();
        let public_key_commitment =
            PublicKeyCommitment::from(secret_key.public_key().to_commitment());
        let account = AccountBuilder::new([0xff; 32])
            .with_auth_component(AuthFalcon512Rpo::new(public_key_commitment))
            .with_component(BasicWallet)
            .build()
            .unwrap();

        // Serialize to JSON
        let json = account.to_json();

        // Deserialize from JSON
        let deserialized_account =
            Account::from_json(&json).expect("Failed to deserialize account");

        // Verify round-trip
        assert_eq!(account.id(), deserialized_account.id());
        assert_eq!(account.nonce(), deserialized_account.nonce());
        assert_eq!(account.commitment(), deserialized_account.commitment());
        assert_eq!(
            account.storage().to_commitment(),
            deserialized_account.storage().to_commitment()
        );
        assert_eq!(
            account.code().commitment(),
            deserialized_account.code().commitment()
        );
    }

    #[test]
    fn signature_scheme_from_accepts_known_values_case_insensitively() {
        assert_eq!(
            SignatureScheme::from("falcon").unwrap(),
            SignatureScheme::Falcon
        );
        assert_eq!(
            SignatureScheme::from("ECDSA").unwrap(),
            SignatureScheme::Ecdsa
        );
    }

    #[test]
    fn signature_scheme_from_rejects_unknown_values() {
        let error = SignatureScheme::from("unknown").unwrap_err();

        assert!(error.contains("unsupported signature scheme"));
    }

    #[test]
    fn signature_scheme_parse_signature_hex_accepts_falcon_signatures() {
        let secret_key = SecretKey::new();
        let message = Word::from([1u32, 2, 3, 4]);
        let signature = secret_key.sign(message);
        let signature_hex = format!("0x{}", ::hex::encode(signature.to_bytes()));

        let parsed = SignatureScheme::Falcon
            .parse_signature_hex(&signature_hex)
            .unwrap();

        assert!(matches!(parsed, AccountSignature::Falcon512Rpo(_)));
    }

    #[test]
    fn signature_scheme_parse_signature_hex_accepts_ecdsa_signatures() {
        let secret_key = EcdsaSecretKey::new();
        let message = Word::from([1u32, 2, 3, 4]);
        let signature = secret_key.sign(message);
        let signature_hex = format!("0x{}", ::hex::encode(signature.to_bytes()));

        let parsed = SignatureScheme::Ecdsa
            .parse_signature_hex(&signature_hex)
            .unwrap();

        assert!(matches!(parsed, AccountSignature::EcdsaK256Keccak(_)));
    }

    #[test]
    fn signature_scheme_build_signature_advice_entry_accepts_falcon_signatures() {
        let secret_key = SecretKey::new();
        let message = Word::from([1u32, 2, 3, 4]);
        let commitment = Word::from([5u32, 6, 7, 8]);
        let signature = AccountSignature::from(secret_key.sign(message));

        let (key, values) = SignatureScheme::Falcon
            .build_signature_advice_entry(commitment, message, &signature, None)
            .unwrap();

        let mut elements = Vec::with_capacity(8);
        elements.extend_from_slice(commitment.as_elements());
        elements.extend_from_slice(message.as_elements());

        assert_eq!(key, Hasher::hash_elements(&elements));
        assert!(!values.is_empty());
    }

    #[test]
    fn signature_scheme_build_signature_advice_entry_accepts_ecdsa_signatures() {
        let secret_key = EcdsaSecretKey::new();
        let public_key = secret_key.public_key();
        let public_key_hex = format!("0x{}", ::hex::encode(public_key.to_bytes()));
        let message = Word::from([1u32, 2, 3, 4]);
        let commitment = public_key.to_commitment();
        let signature = AccountSignature::EcdsaK256Keccak(secret_key.sign(message));

        let (key, values) = SignatureScheme::Ecdsa
            .build_signature_advice_entry(commitment, message, &signature, Some(&public_key_hex))
            .unwrap();

        let mut elements = Vec::with_capacity(8);
        elements.extend_from_slice(commitment.as_elements());
        elements.extend_from_slice(message.as_elements());

        assert_eq!(key, Hasher::hash_elements(&elements));
        assert!(!values.is_empty());
    }

    #[test]
    fn signature_scheme_build_signature_advice_entry_requires_ecdsa_public_key() {
        let secret_key = EcdsaSecretKey::new();
        let message = Word::from([1u32, 2, 3, 4]);
        let commitment = secret_key.public_key().to_commitment();
        let signature = AccountSignature::EcdsaK256Keccak(secret_key.sign(message));

        let error = SignatureScheme::Ecdsa
            .build_signature_advice_entry(commitment, message, &signature, None)
            .unwrap_err();

        assert!(error.contains("requires public key"));
    }
}
