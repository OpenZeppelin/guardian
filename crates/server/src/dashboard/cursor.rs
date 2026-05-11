//! Opaque, tamper-evident pagination cursors for the operator dashboard.
//!
//! Spec reference: `005-operator-dashboard-metrics` FR-005.
//!
//! ## Wire shape
//!
//! Cursors are base64url-encoded payloads of the form:
//!
//! ```text
//! base64url( bincode(payload) || hmac_sha256(secret, bincode(payload)) )
//! ```
//!
//! - The payload is opaque to the client.
//! - The HMAC tag (32 bytes) prevents tampering and prevents clients from
//!   forging cursors.
//! - The signing secret is held by [`CursorSecret`], created once per
//!   server startup. Cursors do not survive a process restart in v1; this
//!   is acceptable for the operator dashboard (operators just request the
//!   first page again). A future feature can introduce a stable
//!   shared-secret rotation strategy if multi-instance deployments need
//!   it (research.md Decision 4).
//!
//! ## Cursor stability
//!
//! For all kinds except [`CursorKind::AccountList`], the cursor encodes
//! the immutable Postgres-assigned `id` of the last entry returned, so
//! traversal is fully stable under both concurrent inserts and concurrent
//! status updates. For [`CursorKind::AccountList`], the cursor encodes
//! the `(updated_at, account_id)` composite key; because `updated_at` is
//! mutable, an account whose `updated_at` is bumped mid-traversal MAY be
//! skipped or repeated — this caveat is documented expected behavior per
//! FR-005.

use base64::Engine;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::error::{GuardianError, Result};

type HmacSha256 = Hmac<Sha256>;

const HMAC_TAG_LEN: usize = 32;
const CURSOR_SECRET_LEN: usize = 32;

/// Identifies which paginated endpoint a cursor was issued for. Decoding
/// rejects cursors whose `kind` does not match the receiving endpoint, so
/// a cursor from `/dashboard/accounts` cannot be replayed on
/// `/dashboard/deltas`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorKind {
    AccountList,
    AccountDeltas,
    AccountProposals,
    GlobalDeltas,
    GlobalProposals,
}

/// Decoded cursor payload. Different kinds use different fields:
///
/// - `AccountList` → `last_updated_at` + `last_account_id` for the
///   composite `(updated_at DESC, account_id ASC)` ordering.
/// - `AccountDeltas` → `last_nonce` (`nonce DESC` against the
///   `(account_id, nonce)` UNIQUE on `deltas`).
/// - `AccountProposals` → `last_nonce` + `last_commitment` (composite
///   `(nonce DESC, commitment DESC)`; nonce alone is not unique on
///   `delta_proposals`).
/// - `GlobalDeltas` → `last_status_timestamp` + `last_account_id` +
///   `last_nonce` for the composite
///   `(status_timestamp DESC, account_id ASC, nonce ASC)` ordering.
/// - `GlobalProposals` → adds `last_commitment` to the global delta
///   tuple for the same uniqueness reason as `AccountProposals`.
///
/// Unused fields are `None` for the kind in question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub kind: CursorKind,
    /// Per-account `nonce` of the last entry on the page just
    /// returned. Used by every kind except `AccountList`.
    pub last_nonce: Option<i64>,
    /// `account_id` of the last entry returned. Used by `AccountList`
    /// (as the `updated_at` tiebreaker) and by both global feeds (as
    /// the `status_timestamp` tiebreaker).
    pub last_account_id: Option<String>,
    /// `updated_at` (account list) or `status_timestamp` (global feeds)
    /// of the last entry returned.
    pub last_updated_at: Option<DateTime<Utc>>,
    /// `commitment` of the last entry returned. Used by the proposal
    /// kinds (`AccountProposals`, `GlobalProposals`) as the stable
    /// tiebreaker when multiple proposals share a `nonce`.
    pub last_commitment: Option<String>,
}

impl Cursor {
    /// Build a cursor for an `AccountList` page resume.
    pub fn account_list(updated_at: DateTime<Utc>, account_id: String) -> Self {
        Self {
            kind: CursorKind::AccountList,
            last_nonce: None,
            last_account_id: Some(account_id),
            last_updated_at: Some(updated_at),
            last_commitment: None,
        }
    }

    /// Build an `AccountDeltas` cursor.
    pub fn account_deltas(last_nonce: i64) -> Self {
        Self {
            kind: CursorKind::AccountDeltas,
            last_nonce: Some(last_nonce),
            last_account_id: None,
            last_updated_at: None,
            last_commitment: None,
        }
    }

    /// Build an `AccountProposals` cursor with the `(nonce, commitment)`
    /// composite tiebreaker.
    pub fn account_proposals(last_nonce: i64, last_commitment: String) -> Self {
        Self {
            kind: CursorKind::AccountProposals,
            last_nonce: Some(last_nonce),
            last_account_id: None,
            last_updated_at: None,
            last_commitment: Some(last_commitment),
        }
    }

    /// Build a `GlobalDeltas` cursor with the
    /// `(status_timestamp, account_id, nonce)` composite key.
    pub fn global_deltas(
        last_status_timestamp: DateTime<Utc>,
        last_account_id: String,
        last_nonce: i64,
    ) -> Self {
        Self {
            kind: CursorKind::GlobalDeltas,
            last_nonce: Some(last_nonce),
            last_account_id: Some(last_account_id),
            last_updated_at: Some(last_status_timestamp),
            last_commitment: None,
        }
    }

    /// Build a `GlobalProposals` cursor with the
    /// `(status_timestamp, account_id, nonce, commitment)` composite
    /// key.
    pub fn global_proposals(
        last_status_timestamp: DateTime<Utc>,
        last_account_id: String,
        last_nonce: i64,
        last_commitment: String,
    ) -> Self {
        Self {
            kind: CursorKind::GlobalProposals,
            last_nonce: Some(last_nonce),
            last_account_id: Some(last_account_id),
            last_updated_at: Some(last_status_timestamp),
            last_commitment: Some(last_commitment),
        }
    }
}

/// Random server-side secret used to HMAC-sign cursors. Generated once
/// per server startup; cursors do not survive a restart.
#[derive(Clone)]
pub struct CursorSecret {
    bytes: [u8; CURSOR_SECRET_LEN],
}

impl CursorSecret {
    /// Generate a fresh random secret. Call this once per server
    /// startup (e.g. at `DashboardState` construction).
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut bytes = [0u8; CURSOR_SECRET_LEN];
        rand::rng().fill_bytes(&mut bytes);
        Self { bytes }
    }

    /// Construct a `CursorSecret` from a fixed byte slice. Mainly useful
    /// in tests where a deterministic secret keeps assertions stable.
    #[cfg(test)]
    pub fn from_bytes(bytes: [u8; CURSOR_SECRET_LEN]) -> Self {
        Self { bytes }
    }

    /// Decode a hex-encoded `CursorSecret` (exactly
    /// `CURSOR_SECRET_LEN * 2` hex chars). Used to wire a stable
    /// shared secret from `GUARDIAN_DASHBOARD_CURSOR_SECRET` so cursors
    /// validate across multi-replica deployments.
    pub fn from_hex(hex_str: &str) -> std::result::Result<Self, String> {
        let trimmed = hex_str.trim();
        let raw = hex::decode(trimmed.strip_prefix("0x").unwrap_or(trimmed))
            .map_err(|e| format!("invalid hex: {e}"))?;
        if raw.len() != CURSOR_SECRET_LEN {
            return Err(format!(
                "expected {CURSOR_SECRET_LEN} bytes, got {}",
                raw.len()
            ));
        }
        let mut bytes = [0u8; CURSOR_SECRET_LEN];
        bytes.copy_from_slice(&raw);
        Ok(Self { bytes })
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::fmt::Debug for CursorSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CursorSecret")
            .field("bytes", &"<redacted>")
            .finish()
    }
}

/// Encode a [`Cursor`] as the opaque base64url string clients see in
/// `next_cursor`. Errors only on bincode serialization failure (which
/// implies an internal bug — the types are statically serializable).
pub fn encode(cursor: &Cursor, secret: &CursorSecret) -> Result<String> {
    let payload = bincode::serialize(cursor).map_err(|e| {
        GuardianError::ConfigurationError(format!("Failed to serialize cursor payload: {e}"))
    })?;
    let mut mac = HmacSha256::new_from_slice(secret.as_slice()).map_err(|e| {
        GuardianError::ConfigurationError(format!("Failed to initialize HMAC: {e}"))
    })?;
    mac.update(&payload);
    let tag = mac.finalize().into_bytes();
    debug_assert_eq!(tag.len(), HMAC_TAG_LEN);

    let mut combined = Vec::with_capacity(payload.len() + HMAC_TAG_LEN);
    combined.extend_from_slice(&payload);
    combined.extend_from_slice(&tag);

    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(combined))
}

/// Decode an opaque cursor string back into a [`Cursor`], verifying the
/// HMAC tag and that the encoded `kind` matches `expected_kind`.
///
/// Returns [`GuardianError::InvalidCursor`] for any of:
///   - malformed base64url
///   - payload too short to contain a tag
///   - HMAC verification failure (tampered or signed with a different
///     secret)
///   - bincode deserialization failure (corrupt payload)
///   - the encoded `kind` does not match `expected_kind` (e.g. an
///     account-list cursor replayed on the deltas endpoint)
///
/// Cursor staleness due to "the data the cursor references no longer
/// exists" is detected at the storage layer, not here. Callers MAY map
/// such storage outcomes to the same `InvalidCursor` error per FR-005.
pub fn decode(encoded: &str, secret: &CursorSecret, expected_kind: CursorKind) -> Result<Cursor> {
    let combined = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded.as_bytes())
        .map_err(|_| GuardianError::InvalidCursor("malformed cursor encoding".to_string()))?;

    if combined.len() <= HMAC_TAG_LEN {
        return Err(GuardianError::InvalidCursor(
            "cursor payload too short".to_string(),
        ));
    }

    let split_at = combined.len() - HMAC_TAG_LEN;
    let payload = &combined[..split_at];
    let tag = &combined[split_at..];

    let mut mac = HmacSha256::new_from_slice(secret.as_slice()).map_err(|e| {
        GuardianError::ConfigurationError(format!("Failed to initialize HMAC: {e}"))
    })?;
    mac.update(payload);
    mac.verify_slice(tag)
        .map_err(|_| GuardianError::InvalidCursor("cursor signature mismatch".to_string()))?;

    let cursor: Cursor = bincode::deserialize(payload)
        .map_err(|_| GuardianError::InvalidCursor("corrupt cursor payload".to_string()))?;

    if cursor.kind != expected_kind {
        return Err(GuardianError::InvalidCursor(format!(
            "cursor kind {:?} does not match expected {:?}",
            cursor.kind, expected_kind
        )));
    }

    Ok(cursor)
}

// ===========================================================================
// T005 — Cursor codec tests
// ===========================================================================
#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_secret() -> CursorSecret {
        CursorSecret::from_bytes([7u8; CURSOR_SECRET_LEN])
    }

    fn other_secret() -> CursorSecret {
        CursorSecret::from_bytes([42u8; CURSOR_SECRET_LEN])
    }

    #[test]
    fn account_list_cursor_roundtrip() {
        let secret = fixed_secret();
        let cursor = Cursor::account_list(
            Utc.with_ymd_and_hms(2026, 5, 8, 12, 0, 0).unwrap(),
            "0xabc".to_string(),
        );
        let encoded = encode(&cursor, &secret).expect("encode");
        let decoded = decode(&encoded, &secret, CursorKind::AccountList).expect("decode");
        assert_eq!(decoded, cursor);
    }

    #[test]
    fn account_deltas_cursor_roundtrip() {
        let secret = fixed_secret();
        let cursor = Cursor::account_deltas(42);
        let encoded = encode(&cursor, &secret).expect("encode");
        let decoded = decode(&encoded, &secret, CursorKind::AccountDeltas).expect("decode");
        assert_eq!(decoded, cursor);
    }

    #[test]
    fn account_proposals_cursor_roundtrip() {
        let secret = fixed_secret();
        let cursor = Cursor::account_proposals(100, "0xdead".to_string());
        let encoded = encode(&cursor, &secret).expect("encode");
        let decoded = decode(&encoded, &secret, CursorKind::AccountProposals).expect("decode");
        assert_eq!(decoded, cursor);
    }

    #[test]
    fn global_deltas_cursor_roundtrip() {
        let secret = fixed_secret();
        let ts = chrono::DateTime::parse_from_rfc3339("2026-05-08T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let cursor = Cursor::global_deltas(ts, "0xacc".to_string(), 9001);
        let encoded = encode(&cursor, &secret).expect("encode");
        let decoded = decode(&encoded, &secret, CursorKind::GlobalDeltas).expect("decode");
        assert_eq!(decoded, cursor);
    }

    #[test]
    fn global_proposals_cursor_roundtrip() {
        let secret = fixed_secret();
        let ts = chrono::DateTime::parse_from_rfc3339("2026-05-08T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let cursor = Cursor::global_proposals(ts, "0xacc".to_string(), 5, "0xdead".to_string());
        let encoded = encode(&cursor, &secret).expect("encode");
        let decoded = decode(&encoded, &secret, CursorKind::GlobalProposals).expect("decode");
        assert_eq!(decoded, cursor);
    }

    #[test]
    fn decode_rejects_kind_mismatch() {
        let secret = fixed_secret();
        let cursor = Cursor::account_deltas(42);
        let encoded = encode(&cursor, &secret).expect("encode");

        let err = decode(&encoded, &secret, CursorKind::GlobalDeltas)
            .expect_err("kind mismatch must fail");
        match err {
            GuardianError::InvalidCursor(msg) => {
                assert!(msg.contains("does not match"), "msg = {msg}");
            }
            other => panic!("expected InvalidCursor, got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_tampered_tag() {
        let secret = fixed_secret();
        let cursor = Cursor::account_deltas(42);
        let encoded = encode(&cursor, &secret).expect("encode");

        // Flip a bit in the encoded base64; this MUST be detected.
        let mut bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded.as_bytes())
            .expect("decode b64");
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        let tampered = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes);

        let err =
            decode(&tampered, &secret, CursorKind::AccountDeltas).expect_err("tamper must fail");
        match err {
            GuardianError::InvalidCursor(msg) => {
                assert!(
                    msg.contains("signature") || msg.contains("corrupt"),
                    "msg = {msg}"
                );
            }
            other => panic!("expected InvalidCursor, got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_foreign_secret() {
        let secret_a = fixed_secret();
        let secret_b = other_secret();
        let cursor = Cursor::account_deltas(42);
        let encoded = encode(&cursor, &secret_a).expect("encode");

        let err = decode(&encoded, &secret_b, CursorKind::AccountDeltas)
            .expect_err("foreign secret must fail");
        match err {
            GuardianError::InvalidCursor(_) => {}
            other => panic!("expected InvalidCursor, got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_malformed_base64() {
        let secret = fixed_secret();
        let err = decode("not_valid_base64!!!", &secret, CursorKind::AccountDeltas)
            .expect_err("malformed base64 must fail");
        match err {
            GuardianError::InvalidCursor(msg) => {
                assert!(msg.contains("malformed"), "msg = {msg}");
            }
            other => panic!("expected InvalidCursor, got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_payload_too_short() {
        let secret = fixed_secret();
        // base64url of 16 zero bytes — well below the HMAC tag length of
        // 32, so even if this somehow decoded as a payload it cannot
        // contain a valid tag.
        let too_short = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 16]);
        let err = decode(&too_short, &secret, CursorKind::AccountDeltas)
            .expect_err("short payload must fail");
        match err {
            GuardianError::InvalidCursor(msg) => {
                assert!(msg.contains("short") || msg.contains("signature"));
            }
            other => panic!("expected InvalidCursor, got {other:?}"),
        }
    }

    #[test]
    fn cursor_secret_debug_does_not_leak_bytes() {
        let secret = fixed_secret();
        let s = format!("{secret:?}");
        assert!(s.contains("redacted"), "Debug must redact bytes: {s}");
        assert!(
            !s.contains("7"),
            "Debug must not leak fixed_secret bytes: {s}"
        );
    }
}
