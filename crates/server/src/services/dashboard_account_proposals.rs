//! Per-account in-flight multisig proposal queue dashboard endpoint
//! service.
//!
//! Spec reference: `005-operator-dashboard-metrics` FR-017..FR-021, US4.
//!
//! Returns the proposals for one account that are still collecting
//! cosigner signatures (`DeltaStatus::Pending` rows in
//! `delta_proposals`). Single-key Miden accounts and EVM
//! (`Auth::EvmEcdsa`) accounts always return an empty paginated result
//! per FR-017 and Decision 8 in `research.md`.
//!
//! Cursor traversal is keyed by `(nonce DESC, commitment DESC)` — both
//! immutable, giving a fully stable contract.

use serde::Serialize;

use crate::dashboard::cursor::{self, Cursor, CursorKind};
use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::error::{GuardianError, Result};
use crate::metadata::AccountMetadata;
use crate::metadata::auth::Auth;
use crate::services::dashboard_pagination::PagedResult;
use crate::state::AppState;
use crate::storage::AccountProposalCursor;

/// One proposal entry in the wire shape per `data-model.md`.
/// `account_id` is omitted on per-account responses; the global
/// proposal feed (Phase 9) wraps this with `account_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DashboardProposalEntry {
    /// Cryptographic identifier cosigners are signing. Per-account
    /// stable identifier.
    pub commitment: String,
    pub nonce: u64,
    pub proposer_id: String,
    pub originating_timestamp: String,
    pub signatures_collected: u32,
    pub signatures_required: u32,
    pub prev_commitment: String,
    /// Hex string when present; `null` for proposals that did not
    /// declare a target commitment.
    pub new_commitment: Option<String>,
    /// Multisig proposal type tag from
    /// `delta_payload.metadata.proposal_type`. In practice this is
    /// always populated for in-flight proposals on this endpoint
    /// (validated on push); the field is `Option` to remain defensive
    /// against legacy or malformed records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_type: Option<String>,
}

impl DashboardProposalEntry {
    /// Build a wire entry from a persisted proposal record. `commitment`
    /// is the storage-layer `delta_proposals.commitment` (the value
    /// cosigners signed); the embedded `DeltaObject` does not preserve
    /// it. Returns `None` for any non-`Pending` status.
    /// `signatures_required` is derived from `auth` per FR-019.
    pub(crate) fn from_record(
        commitment: &str,
        proposal: &DeltaObject,
        auth: &Auth,
    ) -> Option<Self> {
        let DeltaStatus::Pending {
            timestamp,
            proposer_id,
            cosigner_sigs,
        } = &proposal.status
        else {
            return None;
        };
        // FR-021: never expose raw signature bytes — only the count.
        Some(Self {
            commitment: commitment.to_string(),
            nonce: proposal.nonce,
            proposer_id: proposer_id.clone(),
            originating_timestamp: timestamp.clone(),
            signatures_collected: cosigner_sigs.len() as u32,
            signatures_required: signatures_required(auth),
            prev_commitment: proposal.prev_commitment.clone(),
            new_commitment: proposal.new_commitment.clone(),
            proposal_type: proposal.proposal_type().map(str::to_string),
        })
    }
}

/// Derive the `signatures_required` count for a proposal per FR-019.
///
/// - `MidenFalconRpo` / `MidenEcdsa` → number of declared
///   `cosigner_commitments`.
/// - `EvmEcdsa` → never reached on this endpoint; EVM accounts
///   short-circuit to an empty page in [`list_account_proposals`].
///   Returning `0` here is defensive.
pub(crate) fn signatures_required(auth: &Auth) -> u32 {
    match auth {
        Auth::MidenFalconRpo {
            cosigner_commitments,
        }
        | Auth::MidenEcdsa {
            cosigner_commitments,
        } => cosigner_commitments.len() as u32,
        Auth::EvmEcdsa { .. } => 0,
    }
}

/// List in-flight proposals for `account_id`, paginated newest-first
/// by `(nonce DESC, commitment DESC)`.
///
/// Errors:
///   - [`GuardianError::AccountNotFound`] when no metadata exists.
///   - [`GuardianError::DataUnavailable`] when metadata exists but
///     proposal records cannot be loaded (FR-022).
pub async fn list_account_proposals(
    state: &AppState,
    account_id: &str,
    limit: u32,
    cursor: Option<Cursor>,
) -> Result<PagedResult<DashboardProposalEntry>> {
    if let Some(c) = cursor.as_ref()
        && c.kind != CursorKind::AccountProposals
    {
        return Err(GuardianError::InvalidCursor(
            "expected AccountProposals cursor kind".to_string(),
        ));
    }

    let metadata: AccountMetadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| {
            GuardianError::StorageError(format!("Failed to load metadata for '{account_id}': {e}"))
        })?
        .ok_or_else(|| GuardianError::AccountNotFound(account_id.to_string()))?;

    // FR-017: EVM accounts short-circuit to an empty page; their
    // proposal flow does not pass through delta_proposals.
    if matches!(metadata.auth, Auth::EvmEcdsa { .. }) {
        return Ok(PagedResult::empty());
    }

    let storage_cursor = cursor
        .as_ref()
        .and_then(|c| match (c.last_nonce, &c.last_commitment) {
            (Some(last_nonce), Some(last_commitment)) => Some(AccountProposalCursor {
                last_nonce,
                last_commitment: last_commitment.clone(),
            }),
            _ => None,
        });
    let page_size = limit.saturating_add(1);
    let records = state
        .storage
        .list_account_proposals_paged(account_id, page_size, storage_cursor)
        .await
        .map_err(|e| {
            tracing::warn!(
                account_id = %account_id,
                error = %e,
                "dashboard proposal queue could not load proposals"
            );
            GuardianError::DataUnavailable(format!(
                "Failed to load proposal queue for '{account_id}': {e}"
            ))
        })?;

    let mut entries: Vec<DashboardProposalEntry> = records
        .iter()
        .filter_map(|r| {
            DashboardProposalEntry::from_record(&r.commitment, &r.proposal, &metadata.auth)
        })
        .collect();

    let limit_us = limit as usize;
    let has_more = entries.len() > limit_us;
    entries.truncate(limit_us);
    let next_cursor = if has_more {
        entries.last().map(|last| {
            let next = Cursor::account_proposals(last.nonce as i64, last.commitment.clone());
            cursor::encode(&next, state.dashboard.cursor_secret())
        })
    } else {
        None
    }
    .transpose()?;

    Ok(PagedResult::new(entries, next_cursor))
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::delta_object::CosignerSignature;
    use crate::testing::mocks::{MockMetadataStore, MockStorageBackend};
    use std::sync::Arc;

    fn proposal(nonce: u64, commitment: &str, sig_count: usize) -> DeltaObject {
        let cosigner_sigs = (0..sig_count)
            .map(|i| CosignerSignature {
                signature: guardian_shared::ProposalSignature::from_scheme(
                    guardian_shared::SignatureScheme::Falcon,
                    "00".to_string(),
                    None,
                ),
                timestamp: format!("2026-05-08T12:0{i}:00Z"),
                signer_id: format!("0xsigner{i}"),
            })
            .collect();
        DeltaObject {
            account_id: "0xacc".into(),
            nonce,
            prev_commitment: format!("0xprev{nonce}"),
            new_commitment: Some(commitment.to_string()),
            delta_payload: serde_json::json!({}),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Pending {
                timestamp: format!("2026-05-08T12:00:0{nonce}Z"),
                proposer_id: "0xproposer".into(),
                cosigner_sigs,
            },
        }
    }

    fn account_metadata(auth: Auth) -> AccountMetadata {
        AccountMetadata {
            account_id: "0xacc".to_string(),
            auth,
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-01T00:00:00Z".into(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
        }
    }

    async fn state_with_proposals(
        proposals: Vec<DeltaObject>,
        metadata: Option<AccountMetadata>,
        repeat: usize,
    ) -> AppState {
        use crate::ack::AckRegistry;
        use crate::builder::clock::test::MockClock;
        use crate::testing::mocks::MockNetworkClient;
        use tokio::sync::Mutex;

        let mut metadata_store = MockMetadataStore::new();
        for _ in 0..repeat {
            metadata_store = metadata_store.with_get(Ok(metadata.clone()));
        }

        let mut storage = MockStorageBackend::new();
        for _ in 0..repeat {
            let records = proposals
                .iter()
                .map(|p| crate::storage::ProposalRecord {
                    account_id: "0xacc".to_string(),
                    commitment: p
                        .new_commitment
                        .clone()
                        .unwrap_or_else(|| p.prev_commitment.clone()),
                    proposal: p.clone(),
                })
                .collect();
            storage = storage.with_list_account_proposals_paged(Ok(records));
        }

        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");

        AppState {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata_store),
            network_client: Arc::new(Mutex::new(MockNetworkClient::new())),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::default()),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    fn falcon_auth_with(commitments: Vec<&str>) -> Auth {
        Auth::MidenFalconRpo {
            cosigner_commitments: commitments.into_iter().map(String::from).collect(),
        }
    }

    #[tokio::test]
    async fn returns_404_for_unknown_account() {
        let state = state_with_proposals(Vec::new(), None, 1).await;
        let err = list_account_proposals(&state, "0xacc", 50, None)
            .await
            .unwrap_err();
        assert!(matches!(err, GuardianError::AccountNotFound(_)));
    }

    #[tokio::test]
    async fn returns_empty_for_known_multisig_account_with_no_proposals() {
        let state = state_with_proposals(
            Vec::new(),
            Some(account_metadata(falcon_auth_with(vec![
                "0xc1", "0xc2", "0xc3",
            ]))),
            1,
        )
        .await;
        let result = list_account_proposals(&state, "0xacc", 50, None)
            .await
            .unwrap();
        assert!(result.items.is_empty());
        assert!(result.next_cursor.is_none());
    }

    #[tokio::test]
    async fn returns_empty_for_evm_account_per_fr_017() {
        let state = state_with_proposals(
            // Even if the storage layer returned proposals (which it
            // shouldn't for EVM), the service must short-circuit
            // before reading.
            vec![proposal(1, "0xab", 0)],
            Some(account_metadata(Auth::EvmEcdsa {
                signers: vec!["0xsigner".into()],
            })),
            1,
        )
        .await;
        let result = list_account_proposals(&state, "0xacc", 50, None)
            .await
            .unwrap();
        assert!(result.items.is_empty());
        assert!(result.next_cursor.is_none());
    }

    #[tokio::test]
    async fn entries_carry_required_fields() {
        let state = state_with_proposals(
            vec![proposal(7, "0xcommit7", 2)],
            Some(account_metadata(falcon_auth_with(vec![
                "0xc1", "0xc2", "0xc3",
            ]))),
            1,
        )
        .await;
        let result = list_account_proposals(&state, "0xacc", 50, None)
            .await
            .unwrap();
        assert_eq!(result.items.len(), 1);
        let entry = &result.items[0];
        assert_eq!(entry.nonce, 7);
        assert_eq!(entry.commitment, "0xcommit7");
        assert_eq!(entry.proposer_id, "0xproposer");
        assert_eq!(entry.signatures_collected, 2);
        assert_eq!(entry.signatures_required, 3);
        assert_eq!(entry.prev_commitment, "0xprev7");
        assert_eq!(entry.new_commitment.as_deref(), Some("0xcommit7"));
    }

    // Sort/filter behavior moved to the storage layer in feature
    // `005-operator-dashboard-metrics` Decision 1 (revised). Those
    // are exercised by the storage-layer impls and the integration
    // tests in `crates/server/src/api/dashboard_feeds.rs`.

    #[tokio::test]
    async fn rejects_cursor_with_wrong_kind() {
        let state = state_with_proposals(
            Vec::new(),
            Some(account_metadata(falcon_auth_with(vec!["0xc1"]))),
            1,
        )
        .await;
        let wrong = Cursor::account_deltas(5);
        let err = list_account_proposals(&state, "0xacc", 5, Some(wrong))
            .await
            .unwrap_err();
        assert!(matches!(err, GuardianError::InvalidCursor(_)));
    }

    /// FR-022 / SC-012: metadata exists but proposal storage fails
    /// → 503 DataUnavailable (distinct from 404 AccountNotFound).
    #[tokio::test]
    async fn returns_503_when_metadata_exists_but_proposal_storage_fails() {
        use crate::ack::AckRegistry;
        use crate::builder::clock::test::MockClock;
        use crate::testing::mocks::MockNetworkClient;
        use tokio::sync::Mutex;

        let metadata =
            MockMetadataStore::new().with_get(Ok(Some(account_metadata(falcon_auth_with(vec![
                "0xc1", "0xc2",
            ])))));
        // pull_pending_proposals defaults to pull_all_delta_proposals;
        // making the latter fail propagates as DataUnavailable.
        let storage =
            MockStorageBackend::new().with_list_account_proposals_paged(Err("disk dropped".into()));
        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");
        let state = AppState {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata),
            network_client: Arc::new(Mutex::new(MockNetworkClient::new())),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::default()),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        };

        let err = list_account_proposals(&state, "0xacc", 50, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, GuardianError::DataUnavailable(_)),
            "expected DataUnavailable, got {err:?}"
        );
        assert_eq!(err.code(), "data_unavailable");
        assert_eq!(
            err.http_status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn proposal_entries_dont_expose_raw_signatures() {
        let state = state_with_proposals(
            vec![proposal(1, "0xa", 3)],
            Some(account_metadata(falcon_auth_with(vec![
                "0xc1", "0xc2", "0xc3",
            ]))),
            1,
        )
        .await;
        let result = list_account_proposals(&state, "0xacc", 50, None)
            .await
            .unwrap();
        let value = serde_json::to_value(&result.items[0]).unwrap();
        // Wire shape carries counts, not raw bytes.
        assert_eq!(value["signatures_collected"], 3);
        assert_eq!(value["signatures_required"], 3);
        // FR-021: no per-cosigner identity list, no raw signature bytes.
        let object = value.as_object().expect("proposal entry is an object");
        assert!(!object.contains_key("cosigner_sigs"));
        assert!(!object.contains_key("signers"));
        assert!(!object.contains_key("signer_ids"));
    }
}
