pub use guardian_shared::ProposalSignature;
use serde::{Deserialize, Serialize};

/// Cosigner signature entry for delta proposals
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, utoipa::ToSchema)]
pub struct CosignerSignature {
    pub signature: ProposalSignature,
    pub timestamp: String,
    pub signer_id: String,
}

/// Why a delta ended in `Discarded`. Absent on discards that predate
/// the field (and on the worker's retry/divergence discards, which
/// delete the delta instead of transitioning it).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiscardReason {
    /// The client abandoned the candidate via the abandon endpoint
    /// (issue #319) and the worker confirmed the transaction never
    /// landed over the abandon quarantine.
    ClientAbandoned,
}

/// Why a candidate was moved to `Retained` instead of deleted
/// (issue #345). The label is diagnostic — every retained delta is
/// reconciled and TTL-expired identically — but it records which
/// worker verdict parked the row, and a `diverged` row that later
/// reconciles is direct evidence the divergence verdict was spurious
/// (e.g. a lagging RPC node, or the pre-#326 empty-digest misread
/// that hit new accounts' first transactions).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetainReason {
    /// Verification never observed the expected commitment within the
    /// retry budget (RPC outage, worker downtime, slow proving).
    RetryExhausted,
    /// The on-chain commitment was observed at neither the candidate's
    /// base nor its expected commitment on enough consecutive ticks.
    Diverged,
}

/// Delta status state machine
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, utoipa::ToSchema)]
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
        /// Consecutive canonicalization ticks that observed the on-chain
        /// commitment at neither this delta's `prev_commitment` nor its
        /// expected new commitment — i.e. the account advanced past the
        /// state this candidate was built on. Reset to zero whenever a
        /// read shows the account still at the candidate's base, so only
        /// an unbroken streak counts. Requiring more than one observation
        /// before discarding shields against stale RPC reads.
        #[serde(default)]
        divergence_count: u32,
        /// RFC 3339 UTC timestamp of the client's abandon request
        /// (issue #319). While set, the status remains `candidate` — the
        /// account stays locked — until the canonicalization worker
        /// resolves the intent after the abandon quarantine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        abandon_requested_at: Option<String>,
        /// Consecutive worker ticks that observed the on-chain commitment
        /// still at this candidate's base after the abandon was requested.
        /// Reset on a divergent observation, so only an unbroken streak
        /// counts — the same stale-RPC shield as `divergence_count`.
        #[serde(default)]
        abandon_confirm_count: u32,
    },
    Canonical {
        timestamp: String,
    },
    /// A candidate the worker gave up on, kept for background
    /// reconciliation (issue #345) instead of being deleted. A give-up
    /// verdict — retry exhaustion or confirmed divergence — is an
    /// *observation*, not proof the delta is wrong, so the worker keeps
    /// re-checking `stored base + delta` against the chain and promotes
    /// the row if they ever match. Does NOT hold the pending-candidate
    /// lock: new submissions stay unblocked and supersede a retained
    /// row at the same nonce. `timestamp` is when retention began and
    /// anchors the TTL after which the row is dropped for good.
    Retained {
        timestamp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<RetainReason>,
    },
    Discarded {
        timestamp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<DiscardReason>,
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
            divergence_count: 0,
            abandon_requested_at: None,
            abandon_confirm_count: 0,
        }
    }

    pub fn candidate_with_retry(timestamp: String, retry_count: u32) -> Self {
        Self::Candidate {
            timestamp,
            retry_count,
            divergence_count: 0,
            abandon_requested_at: None,
            abandon_confirm_count: 0,
        }
    }

    pub fn canonical(timestamp: String) -> Self {
        Self::Canonical { timestamp }
    }

    pub fn retained(timestamp: String, reason: RetainReason) -> Self {
        Self::Retained {
            timestamp,
            reason: Some(reason),
        }
    }

    pub fn retain_reason(&self) -> Option<RetainReason> {
        match self {
            Self::Retained { reason, .. } => *reason,
            _ => None,
        }
    }

    pub fn discarded(timestamp: String) -> Self {
        Self::Discarded {
            timestamp,
            reason: None,
        }
    }

    pub fn discarded_client_abandoned(timestamp: String) -> Self {
        Self::Discarded {
            timestamp,
            reason: Some(DiscardReason::ClientAbandoned),
        }
    }

    pub fn is_client_abandoned(&self) -> bool {
        matches!(
            self,
            Self::Discarded {
                reason: Some(DiscardReason::ClientAbandoned),
                ..
            }
        )
    }

    pub fn abandon_requested_at(&self) -> Option<&str> {
        match self {
            Self::Candidate {
                abandon_requested_at,
                ..
            } => abandon_requested_at.as_deref(),
            _ => None,
        }
    }

    pub fn abandon_confirm_count(&self) -> u32 {
        match self {
            Self::Candidate {
                abandon_confirm_count,
                ..
            } => *abandon_confirm_count,
            _ => 0,
        }
    }

    /// Record the client's abandon intent, preserving every worker-owned
    /// counter. Idempotent: an already-set request timestamp is kept so
    /// retries cannot restart the quarantine.
    pub fn with_abandon_requested(&self, now: String) -> Self {
        match self {
            Self::Candidate {
                timestamp,
                retry_count,
                divergence_count,
                abandon_requested_at,
                abandon_confirm_count,
            } => Self::Candidate {
                timestamp: timestamp.clone(),
                retry_count: *retry_count,
                divergence_count: *divergence_count,
                abandon_requested_at: Some(abandon_requested_at.clone().unwrap_or(now)),
                abandon_confirm_count: *abandon_confirm_count,
            },
            _ => self.clone(),
        }
    }

    /// Carry a concurrently-recorded abandon request into a status that is
    /// about to overwrite the stored row. Worker counter writes are
    /// computed from a tick-start snapshot, so without this a client
    /// intent recorded mid-tick would be silently wiped. Only
    /// `abandon_requested_at` is preserved — confirmation-streak resets
    /// are legitimate worker writes, and non-candidate statuses drop the
    /// intent by design (terminal states resolve it).
    pub fn with_abandon_request_preserved_from(&self, stored_requested_at: Option<&str>) -> Self {
        match (self, stored_requested_at) {
            (
                Self::Candidate {
                    timestamp,
                    retry_count,
                    divergence_count,
                    abandon_requested_at: None,
                    abandon_confirm_count,
                },
                Some(stored),
            ) => Self::Candidate {
                timestamp: timestamp.clone(),
                retry_count: *retry_count,
                divergence_count: *divergence_count,
                abandon_requested_at: Some(stored.to_string()),
                abandon_confirm_count: *abandon_confirm_count,
            },
            _ => self.clone(),
        }
    }

    pub fn with_incremented_abandon_confirm(&self) -> Self {
        match self {
            Self::Candidate {
                timestamp,
                retry_count,
                divergence_count,
                abandon_requested_at,
                abandon_confirm_count,
            } => Self::Candidate {
                timestamp: timestamp.clone(),
                retry_count: *retry_count,
                divergence_count: *divergence_count,
                abandon_requested_at: abandon_requested_at.clone(),
                abandon_confirm_count: abandon_confirm_count + 1,
            },
            _ => self.clone(),
        }
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

    pub fn is_retained(&self) -> bool {
        matches!(self, Self::Retained { .. })
    }

    pub fn is_discarded(&self) -> bool {
        matches!(self, Self::Discarded { .. })
    }

    pub fn timestamp(&self) -> &str {
        match self {
            Self::Pending { timestamp, .. } => timestamp,
            Self::Candidate { timestamp, .. } => timestamp,
            Self::Canonical { timestamp } => timestamp,
            Self::Retained { timestamp, .. } => timestamp,
            Self::Discarded { timestamp, .. } => timestamp,
        }
    }

    pub fn retry_count(&self) -> u32 {
        match self {
            Self::Candidate { retry_count, .. } => *retry_count,
            _ => 0,
        }
    }

    pub fn divergence_count(&self) -> u32 {
        match self {
            Self::Candidate {
                divergence_count, ..
            } => *divergence_count,
            _ => 0,
        }
    }

    pub fn with_incremented_retry(&self, new_timestamp: String) -> Self {
        match self {
            Self::Candidate {
                timestamp,
                retry_count,
                divergence_count,
                abandon_requested_at,
                abandon_confirm_count,
            } => {
                let _ = new_timestamp;
                Self::Candidate {
                    timestamp: timestamp.clone(),
                    retry_count: retry_count + 1,
                    divergence_count: *divergence_count,
                    abandon_requested_at: abandon_requested_at.clone(),
                    abandon_confirm_count: *abandon_confirm_count,
                }
            }
            _ => self.clone(),
        }
    }

    /// A divergent observation also resets the abandon-confirmation
    /// streak: the account moved, so any prior "still at base" reads no
    /// longer form consecutive evidence that the transaction is dead.
    pub fn with_incremented_divergence(&self) -> Self {
        match self {
            Self::Candidate {
                timestamp,
                retry_count,
                divergence_count,
                abandon_requested_at,
                abandon_confirm_count: _,
            } => Self::Candidate {
                timestamp: timestamp.clone(),
                retry_count: *retry_count,
                divergence_count: divergence_count + 1,
                abandon_requested_at: abandon_requested_at.clone(),
                abandon_confirm_count: 0,
            },
            _ => self.clone(),
        }
    }

    pub fn with_reset_divergence(&self) -> Self {
        match self {
            Self::Candidate {
                timestamp,
                retry_count,
                divergence_count: _,
                abandon_requested_at,
                abandon_confirm_count,
            } => Self::Candidate {
                timestamp: timestamp.clone(),
                retry_count: *retry_count,
                divergence_count: 0,
                abandon_requested_at: abandon_requested_at.clone(),
                abandon_confirm_count: *abandon_confirm_count,
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
            divergence_count: 0,
            abandon_requested_at: None,
            abandon_confirm_count: 0,
        }
    }
}

/// Delta object
#[derive(Serialize, Clone, Debug, Default, utoipa::ToSchema)]
pub struct DeltaObject {
    pub account_id: String,
    pub nonce: u64,
    pub prev_commitment: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_commitment: Option<String>,
    /// Opaque, schema-free JSON payload describing the state delta.
    #[schema(value_type = Object)]
    pub delta_payload: serde_json::Value,
    pub ack_sig: String,
    pub ack_pubkey: String,
    pub ack_scheme: String,
    pub status: DeltaStatus,
    /// Typed dashboard metadata derived at push time. Stored as JSONB
    /// in the `deltas.metadata` column. `None` for EVM deltas and any
    /// historical row never reprocessed by the push-time pipeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<crate::delta_summary::DeltaMetadata>,
}

impl DeltaObject {
    /// Return the multisig proposal type tag carried by this delta.
    ///
    /// Reads from the typed `metadata.proposal` block when present.
    /// Falls back to `delta_payload.metadata.proposal_type` for pending
    /// proposals (the typed column lives only on `deltas`, not
    /// `delta_proposals`) and for historical canonical multisig rows
    /// whose source proposal was already deleted when the push-time
    /// pipeline was introduced.
    pub fn proposal_type(&self) -> Option<&str> {
        if let Some(meta) = &self.metadata
            && let Some(p) = &meta.proposal
        {
            return Some(p.proposal_type.as_str());
        }
        self.delta_payload
            .get("metadata")?
            .get("proposal_type")?
            .as_str()
    }
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
            #[serde(default)]
            metadata: Option<crate::delta_summary::DeltaMetadata>,
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
            metadata: helper.metadata,
        })
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    #[test]
    fn candidate_json_without_abandon_fields_deserializes() {
        // Wire/storage compatibility: candidates persisted before the
        // abandon-intent fields existed must keep deserializing.
        let json = serde_json::json!({
            "status": "candidate",
            "timestamp": "2026-07-01T00:00:00Z",
            "retry_count": 3,
            "divergence_count": 1
        });
        let status: DeltaStatus = serde_json::from_value(json).unwrap();
        assert!(status.is_candidate());
        assert_eq!(status.retry_count(), 3);
        assert_eq!(status.abandon_requested_at(), None);
        assert_eq!(status.abandon_confirm_count(), 0);
    }

    #[test]
    fn discarded_json_without_reason_deserializes() {
        let json = serde_json::json!({
            "status": "discarded",
            "timestamp": "2026-07-01T00:00:00Z"
        });
        let status: DeltaStatus = serde_json::from_value(json).unwrap();
        assert!(status.is_discarded());
        assert!(!status.is_client_abandoned());
    }

    #[test]
    fn retained_status_roundtrips_with_reason() {
        let status = DeltaStatus::retained(
            "2026-07-01T00:00:00Z".to_string(),
            RetainReason::RetryExhausted,
        );
        assert!(status.is_retained());
        assert!(!status.is_candidate());
        assert!(!status.is_discarded());
        assert_eq!(status.timestamp(), "2026-07-01T00:00:00Z");

        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["status"], "retained");
        assert_eq!(json["reason"], "retry_exhausted");
        let back: DeltaStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back.retain_reason(), Some(RetainReason::RetryExhausted));

        let diverged =
            DeltaStatus::retained("2026-07-01T00:00:00Z".to_string(), RetainReason::Diverged);
        let json = serde_json::to_value(&diverged).unwrap();
        assert_eq!(json["reason"], "diverged");
    }

    #[test]
    fn retained_json_without_reason_deserializes() {
        let json = serde_json::json!({
            "status": "retained",
            "timestamp": "2026-07-01T00:00:00Z"
        });
        let status: DeltaStatus = serde_json::from_value(json).unwrap();
        assert!(status.is_retained());
        assert_eq!(status.retain_reason(), None);
    }

    #[test]
    fn client_abandoned_discard_roundtrips() {
        let status = DeltaStatus::discarded_client_abandoned("2026-07-01T00:00:00Z".to_string());
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["reason"], "client_abandoned");
        let back: DeltaStatus = serde_json::from_value(json).unwrap();
        assert!(back.is_client_abandoned());
    }

    #[test]
    fn abandon_request_is_idempotent_and_preserves_counters() {
        let status = DeltaStatus::candidate("2026-07-01T00:00:00Z".to_string())
            .with_incremented_retry("ignored".to_string())
            .with_abandon_requested("2026-07-01T00:01:00Z".to_string());
        assert_eq!(status.retry_count(), 1);
        assert_eq!(status.abandon_requested_at(), Some("2026-07-01T00:01:00Z"));

        // A retried request must not restart the quarantine clock.
        let retried = status.with_abandon_requested("2026-07-01T00:09:00Z".to_string());
        assert_eq!(retried.abandon_requested_at(), Some("2026-07-01T00:01:00Z"));
    }

    #[test]
    fn stored_abandon_request_is_preserved_into_stale_counter_writes() {
        // A worker counter write computed before the intent existed must
        // carry the stored request forward instead of wiping it.
        let stale_write = DeltaStatus::candidate("2026-07-01T00:00:00Z".to_string())
            .with_incremented_divergence();
        let merged = stale_write.with_abandon_request_preserved_from(Some("2026-07-01T00:01:00Z"));
        assert_eq!(merged.abandon_requested_at(), Some("2026-07-01T00:01:00Z"));
        assert_eq!(merged.divergence_count(), 1, "counter write still applies");

        // A write that already carries its own intent keeps it.
        let with_own = DeltaStatus::candidate("2026-07-01T00:00:00Z".to_string())
            .with_abandon_requested("2026-07-01T00:02:00Z".to_string())
            .with_abandon_request_preserved_from(Some("2026-07-01T00:01:00Z"));
        assert_eq!(
            with_own.abandon_requested_at(),
            Some("2026-07-01T00:02:00Z")
        );

        // Terminal statuses drop the intent by design.
        let discarded = DeltaStatus::discarded_client_abandoned("2026-07-01T00:03:00Z".to_string())
            .with_abandon_request_preserved_from(Some("2026-07-01T00:01:00Z"));
        assert_eq!(discarded.abandon_requested_at(), None);
    }

    #[test]
    fn divergent_observation_resets_abandon_confirmations() {
        let status = DeltaStatus::candidate("2026-07-01T00:00:00Z".to_string())
            .with_abandon_requested("2026-07-01T00:01:00Z".to_string())
            .with_incremented_abandon_confirm()
            .with_incremented_abandon_confirm();
        assert_eq!(status.abandon_confirm_count(), 2);

        let diverged = status.with_incremented_divergence();
        assert_eq!(
            diverged.abandon_confirm_count(),
            0,
            "a divergent read breaks the consecutive at-base streak"
        );
        assert_eq!(
            diverged.abandon_requested_at(),
            Some("2026-07-01T00:01:00Z"),
            "the intent itself persists across divergence"
        );
    }

    use super::*;

    #[test]
    fn test_delta_status_deserialization() {
        let json = r#"{"status":"candidate","timestamp":"2025-10-31T21:03:57.489548+00:00"}"#;
        let status: DeltaStatus = serde_json::from_str(json).unwrap();
        assert!(status.is_candidate());
        assert_eq!(status.timestamp(), "2025-10-31T21:03:57.489548+00:00");
    }

    #[test]
    fn proposal_type_reads_from_typed_metadata_when_present() {
        use crate::delta_summary::{
            DashboardDeltaCategory, DeltaMetadata, NoteCounts, ProposalMetadata,
        };
        let delta = DeltaObject {
            metadata: Some(DeltaMetadata {
                category: DashboardDeltaCategory::AssetTransfer,
                assets: Vec::new(),
                counterparty: None,
                note_counts: NoteCounts::default(),
                fee: None,
                proposal: Some(ProposalMetadata {
                    proposal_type: "p2id".to_string(),
                    ..ProposalMetadata::default()
                }),
            }),
            ..Default::default()
        };
        assert_eq!(delta.proposal_type(), Some("p2id"));
    }

    #[test]
    fn proposal_type_falls_back_to_delta_payload_metadata_when_typed_column_is_none() {
        let delta = DeltaObject {
            metadata: None,
            delta_payload: serde_json::json!({
                "tx_summary": { "data": "AAAA" },
                "metadata": {
                    "proposal_type": "consume_notes",
                    "note_ids": ["0xnote1"]
                },
                "signatures": []
            }),
            ..Default::default()
        };
        assert_eq!(delta.proposal_type(), Some("consume_notes"));
    }

    #[test]
    fn proposal_type_returns_none_when_neither_source_has_it() {
        let delta = DeltaObject::default();
        assert!(delta.proposal_type().is_none());
    }

    #[test]
    fn proposal_type_typed_column_wins_over_legacy_path() {
        use crate::delta_summary::{
            DashboardDeltaCategory, DeltaMetadata, NoteCounts, ProposalMetadata,
        };
        let delta = DeltaObject {
            metadata: Some(DeltaMetadata {
                category: DashboardDeltaCategory::AssetTransfer,
                assets: Vec::new(),
                counterparty: None,
                note_counts: NoteCounts::default(),
                fee: None,
                proposal: Some(ProposalMetadata {
                    proposal_type: "p2id".to_string(),
                    ..ProposalMetadata::default()
                }),
            }),
            delta_payload: serde_json::json!({
                "metadata": { "proposal_type": "add_signer" }
            }),
            ..Default::default()
        };
        assert_eq!(delta.proposal_type(), Some("p2id"));
    }

    #[test]
    fn test_delta_object_deserialization() {
        let json = r#"{
            "account_id": "0x4a4a4a4a4a4a4a014a4a4a4a4a4a4a",
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
            divergence_count: 0,
            abandon_requested_at: None,
            abandon_confirm_count: 0,
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
            reason: None,
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
        assert_eq!(incremented.timestamp(), "2024-01-01");

        let incremented_again = incremented.with_incremented_retry("2024-01-03".to_string());
        assert_eq!(incremented_again.retry_count(), 2);
        assert_eq!(incremented_again.timestamp(), "2024-01-01");
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
