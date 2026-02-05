use base64::Engine;
use miden_objects::account::Account;
use miden_objects::transaction::TransactionSummary;
use miden_objects::utils::serde::{Deserializable, Serializable};
use serde::{Deserialize, Serialize};

pub mod auth;
pub mod hex;

/// Supported signature schemes
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignatureScheme {
    Falcon,
    Ecdsa,
}

impl std::fmt::Display for SignatureScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignatureScheme::Falcon => write!(f, "falcon"),
            SignatureScheme::Ecdsa => write!(f, "ecdsa"),
        }
    }
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
    /// Creates a ProposalSignature from a scheme and hex-encoded signature.
    pub fn from_scheme(scheme: SignatureScheme, signature: String) -> Self {
        match scheme {
            SignatureScheme::Falcon => ProposalSignature::Falcon { signature },
            SignatureScheme::Ecdsa => ProposalSignature::Ecdsa {
                signature,
                public_key: None,
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
    use miden_lib::account::{auth::AuthRpoFalcon512, wallets::BasicWallet};
    use miden_objects::{
        account::{AccountBuilder, auth::PublicKeyCommitment},
        crypto::dsa::rpo_falcon512::SecretKey,
    };

    #[test]
    fn test_account_json_round_trip() {
        // Create a test account
        let secret_key = SecretKey::new();
        let public_key_commitment =
            PublicKeyCommitment::from(secret_key.public_key().to_commitment());
        let account = AccountBuilder::new([0xff; 32])
            .with_auth_component(AuthRpoFalcon512::new(public_key_commitment))
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
            account.storage().commitment(),
            deserialized_account.storage().commitment()
        );
        assert_eq!(
            account.code().commitment(),
            deserialized_account.code().commitment()
        );
    }
}
