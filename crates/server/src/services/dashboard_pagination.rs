//! Shared pagination primitives for the operator dashboard endpoints.
//!
//! Spec reference: `005-operator-dashboard-metrics` FR-001..FR-008.
//!
//! - [`PagedResult<T>`] is the response envelope returned by every new
//!   paginated endpoint (account list, per-account feed, global feeds).
//! - [`parse_limit`] applies the documented `[1, 500]` validation with a
//!   default of 50, and treats a bare `?limit=` (present but empty) as
//!   omitted per FR-002.
//! - [`parse_cursor`] decodes an opaque cursor from a query parameter
//!   into a typed [`crate::dashboard::cursor::Cursor`] of the expected
//!   kind, surfacing tampered/foreign cursors as
//!   [`GuardianError::InvalidCursor`].

use serde::{Deserialize, Serialize};

use crate::dashboard::cursor::{self, Cursor, CursorKind, CursorSecret};
use crate::error::{GuardianError, Result};
use crate::state::AppState;
use crate::storage::StorageType;

/// FR-029: filesystem-only threshold guard for cross-account aggregate
/// reads. The Postgres backend serves these from indexed columns and
/// is not bounded by the threshold; only the filesystem backend's
/// fan-out walks every account directory and benefits from the
/// short-circuit. Surfaces as `503 DataUnavailable` at the HTTP layer.
pub async fn enforce_aggregate_threshold(state: &AppState, feed_label: &str) -> Result<()> {
    if state.storage.kind() != StorageType::Filesystem {
        return Ok(());
    }
    let count = state
        .metadata
        .list()
        .await
        .map_err(|e| GuardianError::StorageError(format!("Failed to list account metadata: {e}")))?
        .len();
    let threshold = state.dashboard.filesystem_aggregate_threshold();
    if count > threshold {
        return Err(GuardianError::DataUnavailable(format!(
            "{feed_label}: filesystem inventory size {count} exceeds aggregate threshold {threshold}"
        )));
    }
    Ok(())
}

/// Default page size when `limit` is omitted or supplied without a
/// value.
pub const DEFAULT_LIMIT: u32 = 50;

/// Server-side maximum page size. Requests with `limit > MAX_LIMIT` are
/// rejected with [`GuardianError::InvalidLimit`].
pub const MAX_LIMIT: u32 = 500;

/// Standard cursor-pagination envelope returned by every paginated
/// endpoint. `next_cursor` is `None` at end of list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PagedResult<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

impl<T> PagedResult<T> {
    pub fn new(items: Vec<T>, next_cursor: Option<String>) -> Self {
        Self { items, next_cursor }
    }

    pub fn empty() -> Self {
        Self {
            items: Vec::new(),
            next_cursor: None,
        }
    }
}

/// Parse the optional `limit` query parameter into a clamped page size.
///
/// Behavior per FR-002:
///   - `None` (parameter omitted) → returns [`DEFAULT_LIMIT`].
///   - `Some("")` (bare `?limit=`) → treated as omitted; returns
///     [`DEFAULT_LIMIT`].
///   - `Some(s)` where `s` parses to an integer in `[1, MAX_LIMIT]` →
///     returns that integer.
///   - Anything else → [`GuardianError::InvalidLimit`].
pub fn parse_limit(raw: Option<&str>) -> Result<u32> {
    match raw {
        None | Some("") => Ok(DEFAULT_LIMIT),
        Some(s) => {
            let value: i64 = s.parse().map_err(|_| {
                GuardianError::InvalidLimit(format!(
                    "limit must be a positive integer in [1, {MAX_LIMIT}], got '{s}'"
                ))
            })?;
            if value < 1 {
                return Err(GuardianError::InvalidLimit(format!(
                    "limit must be at least 1, got {value}"
                )));
            }
            if value > MAX_LIMIT as i64 {
                return Err(GuardianError::InvalidLimit(format!(
                    "limit must be at most {MAX_LIMIT}, got {value}"
                )));
            }
            Ok(value as u32)
        }
    }
}

/// Decode the optional `cursor` query parameter into a typed
/// [`Cursor`] of the expected kind. Returns `None` when the parameter
/// is omitted or empty (i.e. start at the first page).
pub fn parse_cursor(
    raw: Option<&str>,
    secret: &CursorSecret,
    expected_kind: CursorKind,
) -> Result<Option<Cursor>> {
    match raw {
        None | Some("") => Ok(None),
        Some(s) => cursor::decode(s, secret, expected_kind).map(Some),
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;

    fn fixed_secret() -> CursorSecret {
        CursorSecret::from_bytes([7u8; 32])
    }

    // --- parse_limit ---

    #[test]
    fn parse_limit_omitted_uses_default() {
        assert_eq!(parse_limit(None).unwrap(), DEFAULT_LIMIT);
    }

    #[test]
    fn parse_limit_bare_empty_string_uses_default() {
        // `?limit=` parses as `Some("")` per axum query semantics.
        assert_eq!(parse_limit(Some("")).unwrap(), DEFAULT_LIMIT);
    }

    #[test]
    fn parse_limit_accepts_in_range_value() {
        assert_eq!(parse_limit(Some("1")).unwrap(), 1);
        assert_eq!(parse_limit(Some("50")).unwrap(), 50);
        assert_eq!(parse_limit(Some("500")).unwrap(), 500);
    }

    #[test]
    fn parse_limit_rejects_zero() {
        let err = parse_limit(Some("0")).unwrap_err();
        assert!(matches!(err, GuardianError::InvalidLimit(_)));
        assert_eq!(err.code(), "invalid_limit");
    }

    #[test]
    fn parse_limit_rejects_negative() {
        let err = parse_limit(Some("-5")).unwrap_err();
        assert!(matches!(err, GuardianError::InvalidLimit(_)));
    }

    #[test]
    fn parse_limit_rejects_above_max() {
        let err = parse_limit(Some("501")).unwrap_err();
        assert!(matches!(err, GuardianError::InvalidLimit(_)));
        assert!(err.to_string().contains("500"));
    }

    #[test]
    fn parse_limit_rejects_far_above_max() {
        let err = parse_limit(Some("9999")).unwrap_err();
        assert!(matches!(err, GuardianError::InvalidLimit(_)));
    }

    #[test]
    fn parse_limit_rejects_non_integer() {
        let err = parse_limit(Some("fifty")).unwrap_err();
        assert!(matches!(err, GuardianError::InvalidLimit(_)));
    }

    #[test]
    fn parse_limit_rejects_decimal() {
        let err = parse_limit(Some("50.0")).unwrap_err();
        assert!(matches!(err, GuardianError::InvalidLimit(_)));
    }

    // --- parse_cursor ---

    #[test]
    fn parse_cursor_omitted_returns_none() {
        let secret = fixed_secret();
        assert!(
            parse_cursor(None, &secret, CursorKind::AccountDeltas)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn parse_cursor_empty_returns_none() {
        let secret = fixed_secret();
        assert!(
            parse_cursor(Some(""), &secret, CursorKind::AccountDeltas)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn parse_cursor_decodes_valid_cursor() {
        let secret = fixed_secret();
        let cursor = Cursor::account_deltas(42);
        let encoded = cursor::encode(&cursor, &secret).unwrap();
        let parsed = parse_cursor(Some(&encoded), &secret, CursorKind::AccountDeltas)
            .unwrap()
            .expect("cursor decoded");
        assert_eq!(parsed, cursor);
    }

    #[test]
    fn parse_cursor_rejects_kind_mismatch() {
        let secret = fixed_secret();
        let cursor = Cursor::account_deltas(42);
        let encoded = cursor::encode(&cursor, &secret).unwrap();
        let err = parse_cursor(Some(&encoded), &secret, CursorKind::GlobalDeltas).unwrap_err();
        assert!(matches!(err, GuardianError::InvalidCursor(_)));
        assert_eq!(err.code(), "invalid_cursor");
    }

    #[test]
    fn parse_cursor_rejects_garbage() {
        let secret = fixed_secret();
        let err = parse_cursor(
            Some("totally not a cursor"),
            &secret,
            CursorKind::AccountDeltas,
        )
        .unwrap_err();
        assert!(matches!(err, GuardianError::InvalidCursor(_)));
    }

    // --- PagedResult ---

    #[test]
    fn paged_result_empty_serializes_with_null_next_cursor() {
        let result: PagedResult<i32> = PagedResult::empty();
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["items"], serde_json::json!([]));
        assert_eq!(json["next_cursor"], serde_json::Value::Null);
    }

    #[test]
    fn paged_result_with_items_and_cursor_serializes_correctly() {
        let result = PagedResult::new(vec![1, 2, 3], Some("abc".to_string()));
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["items"], serde_json::json!([1, 2, 3]));
        assert_eq!(json["next_cursor"], serde_json::json!("abc"));
    }
}
