// Fixture files embedded at compile time
pub const ACCOUNT_JSON: &str = include_str!("account.json");
pub const COMMITMENTS_JSON: &str = include_str!("commitments.json");
pub const DELTA_1_JSON: &str = include_str!("delta_1.json");
pub const DELTA_2_JSON: &str = include_str!("delta_2.json");
pub const DELTA_3_JSON: &str = include_str!("delta_3.json");
pub const PROPOSAL_1_JSON: &str = include_str!("proposal_1.json");
pub const PROPOSAL_2_JSON: &str = include_str!("proposal_2.json");
pub const PROPOSAL_SIGNED_JSON: &str = include_str!("proposal_signed.json");

#[cfg(feature = "e2e")]
pub const KEYS_JSON: &str = include_str!("keys.json");
