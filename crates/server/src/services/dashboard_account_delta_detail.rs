//! Per-delta detail endpoint service.
//!
//! Returns the full structured projection of one canonical delta:
//! dashboard header (status, commitments, persisted metadata),
//! decoded input/output notes, vault changes, and storage changes.
//! Both "unknown account" and "unknown nonce" map to
//! [`GuardianError::DeltaNotFound`] so the wire body is identical.

use serde::Serialize;

use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::delta_summary::{
    DashboardDeltaCategory, DecodeWarning, DecodedNote, ProposalMetadata, StorageChange,
    VaultChange, decode_full as project_detail_sections, decode_transaction_summary,
};
use crate::error::{GuardianError, Result};
use crate::services::dashboard_account_deltas::{DashboardDeltaStatus, decode_delta_status};
use crate::state::AppState;

/// Wire shape for `GET /dashboard/accounts/{account_id}/deltas/{nonce}`.
///
/// `category` and `proposal` are spread to L1 from the persisted
/// metadata column. `note_counts`, `asset`, and `counterparty` are
/// intentionally omitted — they are derivable from the per-section
/// arrays below.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DashboardDeltaDetail {
    pub account_id: String,
    pub nonce: u64,
    pub status: DashboardDeltaStatus,
    pub status_timestamp: String,
    pub prev_commitment: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_commitment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<DashboardDeltaCategory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal: Option<ProposalMetadata>,

    pub input_notes: Vec<DecodedNote>,
    pub output_notes: Vec<DecodedNote>,
    pub vault_changes: Vec<VaultChange>,
    pub storage_changes: Vec<StorageChange>,
    /// Non-empty when one or more sections could not be decoded. The
    /// request still returns 200; affected sections are empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub decode_warnings: Vec<DecodeWarning>,

    /// Base64-encoded raw `TransactionSummary` blob. Present only
    /// when the caller requested `?include=raw` (debug-only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_transaction_summary: Option<String>,
}

/// Flags parsed from the detail endpoint's `?include=` query
/// parameter. Currently only `raw` is supported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DetailIncludeFlags {
    pub raw: bool,
}

pub async fn get_account_delta_detail(
    state: &AppState,
    account_id: &str,
    nonce: u64,
    include: DetailIncludeFlags,
) -> Result<DashboardDeltaDetail> {
    // Unknown account → DeltaNotFound so the body shape matches
    // the unknown-nonce path below.
    let metadata_exists = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| {
            GuardianError::StorageError(format!("Failed to load metadata for '{account_id}': {e}"))
        })?
        .is_some();
    if !metadata_exists {
        return Err(GuardianError::DeltaNotFound {
            account_id: account_id.to_string(),
            nonce,
        });
    }

    let delta = state
        .storage
        .pull_delta(account_id, nonce)
        .await
        .map_err(|err| {
            if crate::storage::is_storage_not_found(&err) {
                GuardianError::DeltaNotFound {
                    account_id: account_id.to_string(),
                    nonce,
                }
            } else {
                tracing::warn!(
                    account_id = %account_id,
                    nonce,
                    error = %err,
                    "dashboard delta detail could not load delta from storage"
                );
                GuardianError::DataUnavailable(format!(
                    "Failed to load delta {nonce} for '{account_id}': {err}"
                ))
            }
        })?;

    Ok(project_delta_to_detail(account_id, nonce, &delta, include))
}

fn project_delta_to_detail(
    account_id: &str,
    nonce: u64,
    delta: &DeltaObject,
    include: DetailIncludeFlags,
) -> DashboardDeltaDetail {
    let (status, retry_count, status_timestamp) = match decode_delta_status(&delta.status) {
        Some(triple) => triple,
        // Defensive: a Pending row leaked into `deltas`. Surface it as
        // Candidate so the caller still gets a response.
        None => fallback_status_triple(&delta.status),
    };

    let (input_notes, output_notes, vault_changes, storage_changes, mut decode_warnings) =
        match decode_transaction_summary(&delta.delta_payload) {
            Ok(summary) => project_detail_sections(&summary),
            Err(reason) => (
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                vec![DecodeWarning {
                    section: crate::delta_summary::DecodeSection::TxSummary,
                    reason: reason.to_string(),
                }],
            ),
        };

    let (category, proposal) = delta
        .metadata
        .as_ref()
        .map(|m| (Some(m.category), m.proposal.clone()))
        .unwrap_or((None, None));

    let raw_transaction_summary = if include.raw {
        let extracted = extract_raw_tx_summary_base64(&delta.delta_payload);
        if extracted.is_none() {
            decode_warnings.push(DecodeWarning {
                section: crate::delta_summary::DecodeSection::TxSummary,
                reason: "raw_unavailable".to_string(),
            });
        }
        extracted
    } else {
        None
    };

    DashboardDeltaDetail {
        account_id: account_id.to_string(),
        nonce,
        status,
        status_timestamp,
        prev_commitment: delta.prev_commitment.clone(),
        new_commitment: delta.new_commitment.clone(),
        retry_count,
        category,
        proposal,
        input_notes,
        output_notes,
        vault_changes,
        storage_changes,
        decode_warnings,
        raw_transaction_summary,
    }
}

/// Return the string at `tx_summary.data` (multisig wrapper) or
/// top-level `data` (raw `push_delta`). Returns `None` when neither
/// location holds a string. Callers must not assume the bytes decode
/// to a Miden `TransactionSummary`.
fn extract_raw_tx_summary_base64(payload: &serde_json::Value) -> Option<String> {
    let candidate = payload.get("tx_summary").unwrap_or(payload);
    candidate
        .get("data")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn fallback_status_triple(status: &DeltaStatus) -> (DashboardDeltaStatus, Option<u32>, String) {
    let timestamp = status.timestamp().to_string();
    (DashboardDeltaStatus::Candidate, Some(0), timestamp)
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::ack::AckRegistry;
    use crate::builder::clock::test::MockClock;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::testing::helpers::create_test_delta_payload;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use axum::http::StatusCode;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    const TEST_ACCOUNT_ID: &str = "0x7bfb0f38b0fafa103f86a805594170";

    fn falcon_metadata() -> AccountMetadata {
        AccountMetadata {
            account_id: TEST_ACCOUNT_ID.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec!["0xc1".into()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-01T00:00:00Z".into(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
            paused_at: None,
            paused_reason: None,
        }
    }

    fn canonical_delta_with_payload(nonce: u64, payload: serde_json::Value) -> DeltaObject {
        DeltaObject {
            account_id: TEST_ACCOUNT_ID.to_string(),
            nonce,
            prev_commitment: format!("0xprev{nonce:04}"),
            new_commitment: Some(format!("0xnew{nonce:04}")),
            delta_payload: payload,
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Canonical {
                timestamp: format!("2026-05-25T08:0{nonce}:00Z"),
            },
            metadata: None,
        }
    }

    async fn build_state(
        metadata_entry: std::result::Result<Option<AccountMetadata>, String>,
        pull_delta_result: std::result::Result<DeltaObject, String>,
    ) -> AppState {
        let metadata = MockMetadataStore::new().with_get(metadata_entry);
        let storage = MockStorageBackend::new().with_pull_delta(pull_delta_result);
        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");
        AppState {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata),
            network_client: Arc::new(Mutex::new(MockNetworkClient::new())),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::default()),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    #[tokio::test]
    async fn unknown_account_returns_delta_not_found_with_matching_shape() {
        let state = build_state(Ok(None), Err("unused".into())).await;
        let err =
            get_account_delta_detail(&state, TEST_ACCOUNT_ID, 42, DetailIncludeFlags::default())
                .await
                .expect_err("unknown account → 404");
        match err {
            GuardianError::DeltaNotFound { account_id, nonce } => {
                assert_eq!(account_id, TEST_ACCOUNT_ID);
                assert_eq!(nonce, 42);
            }
            other => panic!("expected DeltaNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_nonce_returns_delta_not_found_with_matching_shape() {
        let state = build_state(
            Ok(Some(falcon_metadata())),
            Err("Record not found in storage".into()),
        )
        .await;
        let err =
            get_account_delta_detail(&state, TEST_ACCOUNT_ID, 99, DetailIncludeFlags::default())
                .await
                .expect_err("unknown nonce → 404");
        match err {
            GuardianError::DeltaNotFound { account_id, nonce } => {
                assert_eq!(account_id, TEST_ACCOUNT_ID);
                assert_eq!(nonce, 99);
            }
            other => panic!("expected DeltaNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_account_and_unknown_nonce_share_the_same_error_body() {
        let state_unknown_account = build_state(Ok(None), Err("unused".into())).await;
        let state_unknown_nonce =
            build_state(Ok(Some(falcon_metadata())), Err("Record not found".into())).await;
        let err_a = get_account_delta_detail(
            &state_unknown_account,
            TEST_ACCOUNT_ID,
            42,
            DetailIncludeFlags::default(),
        )
        .await
        .unwrap_err();
        let err_b = get_account_delta_detail(
            &state_unknown_nonce,
            TEST_ACCOUNT_ID,
            42,
            DetailIncludeFlags::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(err_a.code(), "delta_not_found");
        assert_eq!(err_a.code(), err_b.code());
        assert_eq!(err_a.http_status(), err_b.http_status());
        let (status_a, body_a) = error_response_parts(err_a).await;
        let (status_b, body_b) = error_response_parts(err_b).await;
        assert_eq!(status_a, status_b);
        assert_eq!(body_a, body_b);
        assert_eq!(
            body_a.get("code").and_then(|v| v.as_str()),
            Some("delta_not_found")
        );
    }

    async fn error_response_parts(err: GuardianError) -> (StatusCode, serde_json::Value) {
        use axum::body::to_bytes;
        use axum::response::IntoResponse;
        let response = err.into_response();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("error body is valid JSON");
        (status, json)
    }

    #[tokio::test]
    async fn real_storage_error_maps_to_data_unavailable() {
        let state = build_state(
            Ok(Some(falcon_metadata())),
            Err("Failed to get connection: pool dropped".into()),
        )
        .await;
        let err =
            get_account_delta_detail(&state, TEST_ACCOUNT_ID, 7, DetailIncludeFlags::default())
                .await
                .expect_err("real storage error → 503");
        assert!(
            matches!(err, GuardianError::DataUnavailable(_)),
            "expected DataUnavailable, got {err:?}"
        );
    }

    #[tokio::test]
    async fn canonical_delta_projects_with_metadata_header_and_decoded_sections() {
        let payload = create_test_delta_payload(TEST_ACCOUNT_ID);
        let mut delta = canonical_delta_with_payload(7, payload);
        delta.metadata = Some(crate::delta_summary::DeltaMetadata {
            category: crate::delta_summary::DashboardDeltaCategory::AccountStorageChange,
            assets: Vec::new(),
            counterparty: None,
            note_counts: crate::delta_summary::NoteCounts::default(),
            proposal: None,
        });
        let state = build_state(Ok(Some(falcon_metadata())), Ok(delta.clone())).await;
        let detail =
            get_account_delta_detail(&state, TEST_ACCOUNT_ID, 7, DetailIncludeFlags::default())
                .await
                .expect("happy path");
        assert_eq!(detail.account_id, TEST_ACCOUNT_ID);
        assert_eq!(detail.nonce, 7);
        assert_eq!(detail.status, DashboardDeltaStatus::Canonical);
        assert_eq!(
            detail.category,
            Some(crate::delta_summary::DashboardDeltaCategory::AccountStorageChange)
        );
        assert!(detail.proposal.is_none());
        assert!(detail.input_notes.is_empty());
        assert!(detail.output_notes.is_empty());
        assert!(detail.vault_changes.is_empty());
        assert!(detail.storage_changes.is_empty());
        assert!(detail.decode_warnings.is_empty());
    }

    #[tokio::test]
    async fn include_raw_attaches_base64_transaction_summary() {
        let payload = create_test_delta_payload(TEST_ACCOUNT_ID);
        let raw_b64 = payload
            .get("data")
            .and_then(|v| v.as_str())
            .expect("test fixture has data field")
            .to_string();
        let delta = canonical_delta_with_payload(11, payload);
        let state = build_state(Ok(Some(falcon_metadata())), Ok(delta)).await;

        let without =
            get_account_delta_detail(&state, TEST_ACCOUNT_ID, 11, DetailIncludeFlags::default())
                .await
                .unwrap();
        assert!(without.raw_transaction_summary.is_none());

        let payload2 = create_test_delta_payload(TEST_ACCOUNT_ID);
        let delta2 = canonical_delta_with_payload(11, payload2);
        let state = build_state(Ok(Some(falcon_metadata())), Ok(delta2)).await;
        let with = get_account_delta_detail(
            &state,
            TEST_ACCOUNT_ID,
            11,
            DetailIncludeFlags { raw: true },
        )
        .await
        .unwrap();
        assert_eq!(with.raw_transaction_summary, Some(raw_b64));
    }

    #[tokio::test]
    async fn include_raw_emits_warning_when_payload_has_no_raw_blob() {
        let delta = canonical_delta_with_payload(12, serde_json::json!({"evm": "0xfeedface"}));
        let state = build_state(Ok(Some(falcon_metadata())), Ok(delta)).await;
        let detail = get_account_delta_detail(
            &state,
            TEST_ACCOUNT_ID,
            12,
            DetailIncludeFlags { raw: true },
        )
        .await
        .expect("response still returns 200");
        assert!(detail.raw_transaction_summary.is_none());
        assert!(
            detail
                .decode_warnings
                .iter()
                .any(|w| w.reason == "raw_unavailable"),
            "expected a raw_unavailable warning, got {:?}",
            detail.decode_warnings,
        );
    }

    #[tokio::test]
    async fn undecodable_payload_returns_200_with_decode_warning() {
        let delta = canonical_delta_with_payload(8, serde_json::json!({"evm": "0xfeedface"}));
        let state = build_state(Ok(Some(falcon_metadata())), Ok(delta)).await;
        let detail =
            get_account_delta_detail(&state, TEST_ACCOUNT_ID, 8, DetailIncludeFlags::default())
                .await
                .expect("EVM-shaped payload still produces a detail response");
        assert!(detail.input_notes.is_empty());
        assert!(detail.output_notes.is_empty());
        assert!(detail.vault_changes.is_empty());
        assert!(detail.storage_changes.is_empty());
        assert_eq!(detail.decode_warnings.len(), 1);
        assert_eq!(
            detail.decode_warnings[0].section,
            crate::delta_summary::DecodeSection::TxSummary,
        );
    }

    /// The `nonce` returned by listing must resolve the same delta
    /// via the detail endpoint.
    #[tokio::test]
    async fn listing_to_detail_round_trip_preserves_nonce() {
        use crate::dashboard::DashboardState;
        use crate::services::list_account_deltas;

        let payload = create_test_delta_payload(TEST_ACCOUNT_ID);
        let seeded_nonce: u64 = 42;
        let seeded_delta = canonical_delta_with_payload(seeded_nonce, payload);

        let storage = MockStorageBackend::new()
            .with_list_account_deltas_paged(Ok(vec![seeded_delta.clone()]))
            .with_pull_delta(Ok(seeded_delta.clone()));
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(falcon_metadata())))
            .with_get(Ok(Some(falcon_metadata())));

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
            dashboard: Arc::new(DashboardState::default()),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        };

        let listing = list_account_deltas(&state, TEST_ACCOUNT_ID, 10, None)
            .await
            .expect("listing succeeds");
        let entry = listing.items.first().expect("at least one entry");
        let listed_nonce = entry.nonce;
        assert_eq!(listed_nonce, seeded_nonce);

        let detail = get_account_delta_detail(
            &state,
            TEST_ACCOUNT_ID,
            listed_nonce,
            DetailIncludeFlags::default(),
        )
        .await
        .expect("detail succeeds for a known nonce");

        assert_eq!(detail.nonce, listed_nonce);
        assert_eq!(detail.account_id, TEST_ACCOUNT_ID);
        assert_eq!(detail.prev_commitment, entry.prev_commitment);
        assert_eq!(detail.new_commitment, entry.new_commitment);
    }
}
