use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    Falcon,
    Ecdsa,
}

impl AuthScheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Falcon => "falcon",
            Self::Ecdsa => "ecdsa",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchmarkAccount {
    pub account_id: String,
    pub owner_user_id: u32,
    pub auth_scheme: AuthScheme,
    pub seeded_commitment: Option<String>,
    pub last_known_commitment: Option<String>,
    pub last_known_nonce: u64,
    pub created_delta_nonces: Vec<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalizationOutcome {
    Canonical,
    Discarded,
    TimedOut,
    ObservationFailed,
}

impl CanonicalizationOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::Discarded => "discarded",
            Self::TimedOut => "timed_out",
            Self::ObservationFailed => "observation_failed",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CanonicalizationSample {
    pub account_id: String,
    pub auth_scheme: AuthScheme,
    pub nonce: u64,
    pub outcome: CanonicalizationOutcome,
    pub wait_ms: f64,
    pub polls: u64,
    pub observation_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchmarkUser {
    pub user_id: u32,
    pub auth_scheme: AuthScheme,
    pub signer_pubkey: String,
    pub account: BenchmarkAccount,
}
