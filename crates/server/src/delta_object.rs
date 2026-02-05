pub use private_state_manager_shared::ProposalSignature;
use serde::{Deserialize, Serialize};

/// Cosigner signature entry for delta proposals
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CosignerSignature {
    pub signature: ProposalSignature,
    pub timestamp: String,
    pub signer_id: String,
}

/// Delta status state machine
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeltaStatus {
    Pending {
        timestamp: String,
        proposer_id: String, // Could be pubkey commitment or other identifier
        cosigner_sigs: Vec<CosignerSignature>,
    },
    Candidate {
        timestamp: String,
        #[serde(default)]
        retry_count: u32,
    },
    Canonical {
        timestamp: String,
    },
    Discarded {
        timestamp: String,
    },
}

impl DeltaStatus {
    pub fn pending(timestamp: String, proposer_id: String) -> Self {
        Self::Pending {
            timestamp,
            proposer_id,
            cosigner_sigs: Vec::new(),
        }
    }

    pub fn candidate(timestamp: String) -> Self {
        Self::Candidate {
            timestamp,
            retry_count: 0,
        }
    }

    pub fn candidate_with_retry(timestamp: String, retry_count: u32) -> Self {
        Self::Candidate {
            timestamp,
            retry_count,
        }
    }

    pub fn canonical(timestamp: String) -> Self {
        Self::Canonical { timestamp }
    }

    pub fn discarded(timestamp: String) -> Self {
        Self::Discarded { timestamp }
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }

    pub fn is_candidate(&self) -> bool {
        matches!(self, Self::Candidate { .. })
    }

    pub fn is_canonical(&self) -> bool {
        matches!(self, Self::Canonical { .. })
    }

    pub fn is_discarded(&self) -> bool {
        matches!(self, Self::Discarded { .. })
    }

    pub fn timestamp(&self) -> &str {
        match self {
            Self::Pending { timestamp, .. } => timestamp,
            Self::Candidate { timestamp, .. } => timestamp,
            Self::Canonical { timestamp } => timestamp,
            Self::Discarded { timestamp } => timestamp,
        }
    }

    pub fn retry_count(&self) -> u32 {
        match self {
            Self::Candidate { retry_count, .. } => *retry_count,
            _ => 0,
        }
    }

    pub fn with_incremented_retry(&self, new_timestamp: String) -> Self {
        match self {
            Self::Candidate { retry_count, .. } => Self::Candidate {
                timestamp: new_timestamp,
                retry_count: retry_count + 1,
            },
            _ => self.clone(),
        }
    }
}

impl Default for DeltaStatus {
    fn default() -> Self {
        Self::Candidate {
            timestamp: String::new(),
            retry_count: 0,
        }
    }
}

/// Delta object
#[derive(Serialize, Clone, Debug, Default)]
pub struct DeltaObject {
    pub account_id: String,
    pub nonce: u64,
    pub prev_commitment: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_commitment: Option<String>,
    pub delta_payload: serde_json::Value,
    pub ack_sig: String,
    pub ack_pubkey: String,
    pub ack_scheme: String,
    pub status: DeltaStatus,
}

impl<'de> Deserialize<'de> for DeltaObject {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        fn nullable_string<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            Option::<String>::deserialize(deserializer).map(|opt| opt.unwrap_or_default())
        }

        #[derive(Deserialize)]
        struct DeltaObjectHelper {
            account_id: String,
            nonce: u64,
            prev_commitment: String,
            new_commitment: Option<String>,
            delta_payload: serde_json::Value,
            #[serde(default, deserialize_with = "nullable_string")]
            ack_sig: String,
            #[serde(default, deserialize_with = "nullable_string")]
            ack_pubkey: String,
            #[serde(default, deserialize_with = "nullable_string")]
            ack_scheme: String,
            #[serde(default)]
            status: Option<DeltaStatus>,
            #[serde(default)]
            candidate_at: Option<String>,
            #[serde(default)]
            canonical_at: Option<String>,
            #[serde(default)]
            discarded_at: Option<String>,
        }

        let helper = DeltaObjectHelper::deserialize(deserializer)?;

        let status = if let Some(status) = helper.status {
            status
        } else if let Some(discarded_at) = helper.discarded_at {
            DeltaStatus::discarded(discarded_at)
        } else if let Some(canonical_at) = helper.canonical_at {
            DeltaStatus::canonical(canonical_at)
        } else if let Some(candidate_at) = helper.candidate_at {
            DeltaStatus::candidate(candidate_at)
        } else {
            DeltaStatus::default()
        };

        Ok(DeltaObject {
            account_id: helper.account_id,
            nonce: helper.nonce,
            prev_commitment: helper.prev_commitment,
            new_commitment: helper.new_commitment,
            delta_payload: helper.delta_payload,
            ack_sig: helper.ack_sig,
            ack_pubkey: helper.ack_pubkey,
            ack_scheme: helper.ack_scheme,
            status,
        })
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;

    #[test]
    fn test_delta_status_deserialization() {
        let json = r#"{"status":"candidate","timestamp":"2025-10-31T21:03:57.489548+00:00"}"#;
        let status: DeltaStatus = serde_json::from_str(json).unwrap();
        assert!(status.is_candidate());
        assert_eq!(status.timestamp(), "2025-10-31T21:03:57.489548+00:00");
    }

    #[test]
    fn test_delta_object_deserialization() {
        let json = r#"{
            "account_id": "0x2f02fa4c9e787b101bf02bc266db39",
            "nonce": 0,
            "prev_commitment": "0xdc2820847638d1f15f174ea0657e3228e5b7774be44be1e608e4c64d92eaaaeb",
            "new_commitment": "0x8fa68eabc9817e17900a7f1f705c1ecdeef6ab64c15ca1b66447272fb8fa49b2",
            "delta_payload": {},
            "ack_sig": null,
            "status": {
                "status": "candidate",
                "timestamp": "2025-10-31T21:03:57.489548+00:00"
            }
        }"#;

        let delta: DeltaObject = serde_json::from_str(json).unwrap();
        assert_eq!(delta.nonce, 0);
        assert!(delta.status.is_candidate());
    }

    #[test]
    fn test_delta_status_constructors() {
        let pending = DeltaStatus::pending("2024-01-01".to_string(), "proposer1".to_string());
        assert!(pending.is_pending());
        assert_eq!(pending.timestamp(), "2024-01-01");

        let candidate = DeltaStatus::candidate("2024-01-02".to_string());
        assert!(candidate.is_candidate());
        assert_eq!(candidate.timestamp(), "2024-01-02");

        let canonical = DeltaStatus::canonical("2024-01-03".to_string());
        assert!(canonical.is_canonical());
        assert_eq!(canonical.timestamp(), "2024-01-03");

        let discarded = DeltaStatus::discarded("2024-01-04".to_string());
        assert!(discarded.is_discarded());
        assert_eq!(discarded.timestamp(), "2024-01-04");
    }

    #[test]
    fn test_delta_status_is_methods() {
        let pending = DeltaStatus::Pending {
            timestamp: "2024-01-01".to_string(),
            proposer_id: "p1".to_string(),
            cosigner_sigs: vec![],
        };
        assert!(pending.is_pending());
        assert!(!pending.is_candidate());
        assert!(!pending.is_canonical());
        assert!(!pending.is_discarded());

        let candidate = DeltaStatus::Candidate {
            timestamp: "2024-01-02".to_string(),
            retry_count: 0,
        };
        assert!(!candidate.is_pending());
        assert!(candidate.is_candidate());
        assert!(!candidate.is_canonical());
        assert!(!candidate.is_discarded());

        let canonical = DeltaStatus::Canonical {
            timestamp: "2024-01-03".to_string(),
        };
        assert!(!canonical.is_pending());
        assert!(!canonical.is_candidate());
        assert!(canonical.is_canonical());
        assert!(!canonical.is_discarded());

        let discarded = DeltaStatus::Discarded {
            timestamp: "2024-01-04".to_string(),
        };
        assert!(!discarded.is_pending());
        assert!(!discarded.is_candidate());
        assert!(!discarded.is_canonical());
        assert!(discarded.is_discarded());
    }

    #[test]
    fn test_delta_status_default() {
        let status = DeltaStatus::default();
        assert!(status.is_candidate());
        assert_eq!(status.timestamp(), "");
    }

    #[test]
    fn test_delta_object_deserialization_legacy_candidate_at() {
        let json = r#"{
            "account_id": "0x123",
            "nonce": 1,
            "prev_commitment": "0xabc",
            "new_commitment": "0xdef",
            "delta_payload": {},
            "ack_sig": null,
            "candidate_at": "2024-01-01T00:00:00Z"
        }"#;

        let delta: DeltaObject = serde_json::from_str(json).unwrap();
        assert!(delta.status.is_candidate());
        assert_eq!(delta.status.timestamp(), "2024-01-01T00:00:00Z");
    }

    #[test]
    fn test_delta_object_deserialization_legacy_canonical_at() {
        let json = r#"{
            "account_id": "0x123",
            "nonce": 1,
            "prev_commitment": "0xabc",
            "new_commitment": "0xdef",
            "delta_payload": {},
            "ack_sig": null,
            "canonical_at": "2024-01-02T00:00:00Z"
        }"#;

        let delta: DeltaObject = serde_json::from_str(json).unwrap();
        assert!(delta.status.is_canonical());
        assert_eq!(delta.status.timestamp(), "2024-01-02T00:00:00Z");
    }

    #[test]
    fn test_delta_object_deserialization_legacy_discarded_at() {
        let json = r#"{
            "account_id": "0x123",
            "nonce": 1,
            "prev_commitment": "0xabc",
            "new_commitment": null,
            "delta_payload": {},
            "ack_sig": null,
            "discarded_at": "2024-01-03T00:00:00Z"
        }"#;

        let delta: DeltaObject = serde_json::from_str(json).unwrap();
        assert!(delta.status.is_discarded());
        assert_eq!(delta.status.timestamp(), "2024-01-03T00:00:00Z");
    }

    #[test]
    fn test_delta_object_deserialization_no_status() {
        let json = r#"{
            "account_id": "0x123",
            "nonce": 1,
            "prev_commitment": "0xabc",
            "new_commitment": "0xdef",
            "delta_payload": {},
            "ack_sig": null
        }"#;

        let delta: DeltaObject = serde_json::from_str(json).unwrap();
        assert!(delta.status.is_candidate());
        assert_eq!(delta.status.timestamp(), "");
    }

    #[test]
    fn test_cosigner_signature() {
        let sig = CosignerSignature {
            signature: ProposalSignature::Falcon {
                signature: "0xabc".to_string(),
            },
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            signer_id: "signer1".to_string(),
        };

        let json = serde_json::to_string(&sig).unwrap();
        let deserialized: CosignerSignature = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, sig);
    }

    #[test]
    fn test_candidate_retry_count() {
        let candidate = DeltaStatus::candidate("2024-01-01".to_string());
        assert_eq!(candidate.retry_count(), 0);

        let candidate_with_retry = DeltaStatus::candidate_with_retry("2024-01-01".to_string(), 5);
        assert_eq!(candidate_with_retry.retry_count(), 5);

        let incremented = candidate.with_incremented_retry("2024-01-02".to_string());
        assert_eq!(incremented.retry_count(), 1);
        assert_eq!(incremented.timestamp(), "2024-01-02");

        let incremented_again = incremented.with_incremented_retry("2024-01-03".to_string());
        assert_eq!(incremented_again.retry_count(), 2);
    }

    #[test]
    fn test_retry_count_for_non_candidate() {
        let canonical = DeltaStatus::canonical("2024-01-01".to_string());
        assert_eq!(canonical.retry_count(), 0);

        let pending = DeltaStatus::pending("2024-01-01".to_string(), "proposer".to_string());
        assert_eq!(pending.retry_count(), 0);
    }

    #[test]
    fn test_candidate_deserialization_without_retry_count() {
        let json = r#"{"status":"candidate","timestamp":"2024-01-01T00:00:00Z"}"#;
        let status: DeltaStatus = serde_json::from_str(json).unwrap();
        assert!(status.is_candidate());
        assert_eq!(status.retry_count(), 0);
    }

    #[test]
    fn test_candidate_deserialization_with_retry_count() {
        let json = r#"{"status":"candidate","timestamp":"2024-01-01T00:00:00Z","retry_count":3}"#;
        let status: DeltaStatus = serde_json::from_str(json).unwrap();
        assert!(status.is_candidate());
        assert_eq!(status.retry_count(), 3);
    }
}
