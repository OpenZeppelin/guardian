// Fixture files embedded at compile time
pub const ACCOUNT_JSON: &str = include_str!("account.json");
pub const COMMITMENTS_JSON: &str = include_str!("commitments.json");
pub const DELTA_1_JSON: &str = include_str!("delta_1.json");
pub const DELTA_2_JSON: &str = include_str!("delta_2.json");
pub const DELTA_3_JSON: &str = include_str!("delta_3.json");
/// A second chain from `account.json`, `queue_1` -> `queue_2` -> `queue_3`,
/// of threshold-only deltas: the signer set and the guardian key stay as
/// created, so these can be queued behind one another (issue #17). The
/// `delta_N` chain cannot: `delta_1` and `delta_2` each add a signer, and
/// nothing chains behind a candidate that changes the signer set. Derived
/// by `generate_roster_preserving_queue_fixtures`.
pub const QUEUE_1_JSON: &str = include_str!("queue_1.json");
pub const QUEUE_2_JSON: &str = include_str!("queue_2.json");
pub const QUEUE_3_JSON: &str = include_str!("queue_3.json");
pub const PROPOSAL_1_JSON: &str = include_str!("proposal_1.json");
pub const PROPOSAL_2_JSON: &str = include_str!("proposal_2.json");
pub const PROPOSAL_SIGNED_JSON: &str = include_str!("proposal_signed.json");
pub const KEYS_JSON: &str = include_str!("keys.json");
