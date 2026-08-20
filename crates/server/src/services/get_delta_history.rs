//! Client-facing per-account transaction history (issue #413).
//!
//! Exposes the canonical delta history Guardian already retains as a
//! paginated, cosigner-authenticated feed with decoded input/output
//! note summaries, so a wallet can render an account's history after
//! recovery. Read-only: served while the account is paused, like the
//! other client read endpoints (see [`crate::services::account_status`]).
//!
//! Only `canonical` deltas appear — candidates are still in flight and
//! retained/discarded rows never became part of the account state.
//! Cursor traversal is fully stable: `nonce` is per-account immutable
//! and monotonic, same argument as the dashboard delta feed.

use serde::Serialize;

use crate::dashboard::cursor::{self, Cursor, CursorKind};
use crate::delta_object::DeltaObject;
use crate::delta_summary::{DecodeWarning, DecodedNote, decode_full, decode_transaction_summary};
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::services::dashboard_pagination::PagedResult;
use crate::services::resolve_account;
use crate::state::AppState;
use crate::storage::AccountDeltaCursor;

#[derive(Debug, Clone)]
pub struct GetDeltaHistoryParams {
    pub account_id: String,
    pub limit: u32,
    pub cursor: Option<Cursor>,
    pub credentials: Credentials,
}

/// One canonical transaction in the history feed wire shape.
/// `account_id` is omitted — the authenticated query scopes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct HistoryEntry {
    pub nonce: u64,
    /// RFC 3339 UTC timestamp at which the delta became canonical.
    pub timestamp: String,
    /// Account commitment after this transaction; lets a wallet
    /// correlate history entries with recovered state. `null` when the
    /// stored row predates commitment recording.
    pub new_commitment: Option<String>,
    pub input_notes: Vec<DecodedNote>,
    pub output_notes: Vec<DecodedNote>,
    /// Why the note sections are empty when they are: the persisted
    /// payload could not be decoded (e.g. rows predating the current
    /// summary format). The entry itself is still returned rather than
    /// failing the page.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub decode_warnings: Vec<DecodeWarning>,
}

impl HistoryEntry {
    /// Project a persisted canonical [`DeltaObject`] into the history
    /// wire shape, reusing the dashboard note projection. A payload
    /// that fails to decode yields empty sections plus a warning —
    /// one undecodable row must not 500 the whole page.
    fn from_delta(delta: &DeltaObject) -> Self {
        let (input_notes, output_notes, decode_warnings) =
            match decode_transaction_summary(&delta.delta_payload) {
                Ok(summary) => {
                    let (input_notes, output_notes, _vault, _storage, warnings) =
                        decode_full(&summary);
                    (input_notes, output_notes, warnings)
                }
                Err(reason) => (
                    Vec::new(),
                    Vec::new(),
                    vec![DecodeWarning {
                        section: crate::delta_summary::DecodeSection::TxSummary,
                        reason: reason.to_string(),
                    }],
                ),
            };
        Self {
            nonce: delta.nonce,
            timestamp: delta.status.timestamp().to_string(),
            new_commitment: delta.new_commitment.clone(),
            input_notes,
            output_notes,
            decode_warnings,
        }
    }
}

/// List the canonical transaction history for an account, paginated
/// newest-first by `nonce DESC`.
///
/// Errors:
///   - [`GuardianError::AccountNotFound`] / `AuthenticationFailed` /
///     `UnsupportedForNetwork` from [`resolve_account`].
///   - [`GuardianError::InvalidCursor`] when the cursor is not an
///     `AccountHistory` cursor (parse failures surface at the
///     transport layer).
///   - [`GuardianError::StorageError`] when the delta rows cannot be
///     loaded.
#[tracing::instrument(
    skip(state, params),
    fields(account_id = %params.account_id, limit = params.limit)
)]
pub async fn get_delta_history(
    state: &AppState,
    params: GetDeltaHistoryParams,
) -> Result<PagedResult<HistoryEntry>> {
    // An AccountHistory cursor carries `last_nonce` plus the account
    // it was minted for. A kind-valid cursor without a resume position
    // must be rejected rather than silently restarting from page 1 (a
    // resume loop would then re-read the first page forever), and a
    // cursor minted for a different account must not be replayed here.
    if let Some(c) = params.cursor.as_ref()
        && (c.kind != CursorKind::AccountHistory
            || c.last_nonce.is_none()
            || c.last_account_id.as_deref() != Some(params.account_id.as_str()))
    {
        return Err(GuardianError::InvalidCursor(
            "expected AccountHistory cursor minted for this account".to_string(),
        ));
    }

    let resolved = resolve_account(state, &params.account_id, &params.credentials).await?;

    // Fetch one extra row so `next_cursor` is emitted only when more
    // rows actually exist.
    let storage_cursor = params.cursor.as_ref().and_then(|c| {
        c.last_nonce
            .map(|last_nonce| AccountDeltaCursor { last_nonce })
    });
    let page_size = params.limit.saturating_add(1);
    let rows = resolved
        .storage
        .list_canonical_deltas_paged(&params.account_id, page_size, storage_cursor)
        .await
        .map_err(|e| {
            tracing::warn!(
                account_id = %params.account_id,
                error = %e,
                "history feed could not load canonical deltas"
            );
            GuardianError::StorageError(format!(
                "Failed to load history for '{}': {e}",
                params.account_id
            ))
        })?;

    // Truncate to the page before decoding: the sentinel row exists
    // only to answer has-more and must not pay a note decode.
    let mut rows = rows;
    let limit_us = params.limit as usize;
    let has_more = rows.len() > limit_us;
    rows.truncate(limit_us);

    let entries: Vec<HistoryEntry> = rows.iter().map(HistoryEntry::from_delta).collect();

    let next_cursor = if has_more {
        entries.last().map(|last| {
            let next = Cursor::account_history(last.nonce as i64, params.account_id.clone());
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
    use crate::delta_object::DeltaStatus;

    fn canonical(nonce: u64) -> DeltaObject {
        DeltaObject {
            account_id: "0xacc".to_string(),
            nonce,
            prev_commitment: format!("0xprev{nonce}"),
            new_commitment: Some(format!("0xnew{nonce}")),
            delta_payload: serde_json::json!({}),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Canonical {
                timestamp: format!("2026-08-01T12:0{nonce}:00Z"),
            },
            metadata: None,
        }
    }

    #[test]
    fn from_delta_surfaces_decode_warning_for_undecodable_payload() {
        let entry = HistoryEntry::from_delta(&canonical(3));
        assert_eq!(entry.nonce, 3);
        assert_eq!(entry.timestamp, "2026-08-01T12:03:00Z");
        assert_eq!(entry.new_commitment.as_deref(), Some("0xnew3"));
        assert!(entry.input_notes.is_empty());
        assert!(entry.output_notes.is_empty());
        assert_eq!(entry.decode_warnings.len(), 1);
        assert_eq!(
            entry.decode_warnings[0].section,
            crate::delta_summary::DecodeSection::TxSummary
        );
    }

    #[test]
    fn serialized_entry_omits_empty_decode_warnings() {
        let mut entry = HistoryEntry::from_delta(&canonical(1));
        entry.decode_warnings.clear();
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("decode_warnings").is_none());
        assert_eq!(json["nonce"], serde_json::json!(1));
        assert_eq!(json["new_commitment"], serde_json::json!("0xnew1"));
    }

    #[tokio::test]
    async fn rejects_cursor_with_wrong_kind() {
        use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
        use std::sync::Arc;

        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = crate::ack::AckRegistry::new(keystore_dir)
            .await
            .expect("ack");
        let state = AppState {
            storage: Arc::new(MockStorageBackend::new()),
            metadata: Arc::new(MockMetadataStore::new()),
            network_client: Arc::new(MockNetworkClient::new()),
            ack,
            canonicalization: None,
            clock: Arc::new(crate::builder::clock::test::MockClock::default()),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        };

        let params = GetDeltaHistoryParams {
            account_id: "0xacc".to_string(),
            limit: 50,
            cursor: Some(Cursor::account_deltas(5)),
            credentials: Credentials::signature(String::new(), String::new(), 0),
        };
        let err = get_delta_history(&state, params.clone()).await.unwrap_err();
        assert!(matches!(err, GuardianError::InvalidCursor(_)));

        // Kind-valid cursor without a resume position must be rejected,
        // not silently degraded to "first page".
        let empty = Cursor {
            kind: CursorKind::AccountHistory,
            last_nonce: None,
            last_account_id: Some("0xacc".to_string()),
            last_updated_at: None,
            last_commitment: None,
        };
        let err = get_delta_history(
            &state,
            GetDeltaHistoryParams {
                cursor: Some(empty),
                ..params.clone()
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, GuardianError::InvalidCursor(_)));

        // A cursor minted for another account must not resume this one.
        let foreign = Cursor::account_history(5, "0xother".to_string());
        let err = get_delta_history(
            &state,
            GetDeltaHistoryParams {
                cursor: Some(foreign),
                ..params
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, GuardianError::InvalidCursor(_)));
    }
}
