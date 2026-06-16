use serde::{Deserialize, Serialize};

pub(crate) const ENVELOPE_VERSION: u8 = 1;
pub(crate) const ALG_AES_256_GCM: &str = "AES-256-GCM";

/// At-rest representation of one encrypted payload, stored in place of the
/// plaintext in the existing `jsonb` column / filesystem JSON file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Envelope {
    pub(crate) v: u8,
    pub(crate) alg: String,
    pub(crate) kid: String,
    pub(crate) nonce: String,
    pub(crate) ct: String,
}

/// Identity a ciphertext is bound to via AEAD additional authenticated data,
/// reconstructed from a record's plaintext routing fields at decrypt time so a
/// ciphertext cannot be relocated to a different record undetected.
pub(crate) enum RecordAad<'a> {
    State {
        account_id: &'a str,
    },
    Delta {
        account_id: &'a str,
        nonce: u64,
    },
    Proposal {
        account_id: &'a str,
        commitment: &'a str,
    },
}

impl RecordAad<'_> {
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        match self {
            RecordAad::State { account_id } => format!("state:{account_id}"),
            RecordAad::Delta { account_id, nonce } => format!("delta:{account_id}:{nonce}"),
            RecordAad::Proposal {
                account_id,
                commitment,
            } => format!("proposal:{account_id}:{commitment}"),
        }
        .into_bytes()
    }
}
