//! Service for the `/state/lookup` endpoint.
//!
//! Resolves a Miden public-key commitment to the set of account IDs whose
//! authorization set contains that commitment. Authentication is by
//! proof-of-possession: the caller submits a signature over a
//! `LookupAuthMessage` digest; the service derives the public key from the
//! signature itself (Falcon embeds it; ECDSA recovers it), verifies the
//! signature, and requires the derived commitment to equal the queried
//! commitment.
//!
//! The `x-pubkey` header is part of the wire format for consistency with
//! per-account requests but is not consulted on this path — identity is
//! sourced from the signature.
//!
//! The service is intentionally account-less. It does NOT call
//! `services::resolve_account` because that path requires an `account_id`
//! and applies per-account replay protection — both inappropriate here, since
//! the caller is trying to discover the account ID and there is no per-account
//! `last_auth_timestamp` to compare against.

use crate::error::{GuardianError, Result};
use crate::metadata::auth::lookup::{commitment_of, derive_pubkey_from_lookup_signature};
use crate::metadata::auth::{Credentials, MAX_TIMESTAMP_SKEW_MS};
use crate::state::AppState;
use guardian_shared::hex::FromHex;
use miden_protocol::Word;

/// Length of a Miden public-key commitment in hex characters (32 bytes ×
/// 2 chars/byte). Excludes the `0x` prefix.
const COMMITMENT_HEX_CHARS: usize = 64;

#[derive(Debug, Clone)]
pub struct LookupAccountParams {
    pub key_commitment: String,
    /// Standard request credentials. The `pubkey` field is not consulted on
    /// the lookup path — the public key is derived from the signature itself
    /// (Falcon embeds it; ECDSA recovers it). The field is part of the wire
    /// format only for API consistency with per-account requests.
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct LookupAccountResult {
    /// Account IDs whose authorization set contains `key_commitment`. May be
    /// empty: a successful proof-of-possession against a commitment that no
    /// account authorizes returns an empty list (200 OK), not a not-found
    /// error. This intentionally does not distinguish "no account exists for
    /// this commitment" from "an account exists but the caller does not hold
    /// the key" — the latter fails authentication first.
    pub accounts: Vec<String>,
}

#[tracing::instrument(
    skip(state, params),
    fields(key_commitment = %params.key_commitment)
)]
pub async fn lookup_account(
    state: &AppState,
    params: LookupAccountParams,
) -> Result<LookupAccountResult> {
    tracing::info!(key_commitment = %params.key_commitment, "Looking up accounts by key commitment");

    let normalized_commitment = normalize_commitment_hex(&params.key_commitment)?;
    let key_commitment_word = Word::from_hex(&normalized_commitment)
        .map_err(|e| GuardianError::InvalidInput(format!("invalid key_commitment: {e}")))?;

    // No per-account last_auth_timestamp check: the account is unknown here.
    // Replays within the skew window are accepted by design.
    let request_timestamp = params.credentials.timestamp();
    let server_now_ms = state.clock.now().timestamp_millis();
    let time_diff_ms = (server_now_ms - request_timestamp).abs();
    if time_diff_ms > MAX_TIMESTAMP_SKEW_MS {
        tracing::warn!(
            request_timestamp = %request_timestamp,
            server_now_ms = %server_now_ms,
            time_diff_ms = %time_diff_ms,
            max_skew_ms = %MAX_TIMESTAMP_SKEW_MS,
            "Lookup request timestamp outside allowed skew window"
        );
        return Err(GuardianError::AuthenticationFailed(format!(
            "Request timestamp outside allowed window: {time_diff_ms}ms drift (max {MAX_TIMESTAMP_SKEW_MS}ms)"
        )));
    }

    let (_pubkey_hex, signature_hex, _) = params.credentials.as_signature().ok_or_else(|| {
        GuardianError::AuthenticationFailed("missing signature credentials".into())
    })?;

    // Derive the pubkey from the signature itself (Falcon embeds it; ECDSA
    // recovers it). LookupAuthMessage is domain-separated from
    // AuthRequestMessage by construction (see lookup_auth_message.rs).
    let parsed_pubkey =
        derive_pubkey_from_lookup_signature(signature_hex, request_timestamp, key_commitment_word)
            .map_err(GuardianError::AuthenticationFailed)?;

    // Bind the recovered key to the queried commitment: this is the
    // proof-of-possession check. Knowing only the queried commitment is not
    // enough to forge a signature whose embedded/recovered pubkey hashes to
    // that same commitment.
    let derived_commitment = commitment_of(&parsed_pubkey);
    if derived_commitment != normalized_commitment {
        tracing::warn!(
            queried_commitment = %normalized_commitment,
            derived_commitment = %derived_commitment,
            "Lookup signature key does not match queried commitment"
        );
        return Err(GuardianError::AuthenticationFailed(
            "signature key does not derive to the queried key_commitment".into(),
        ));
    }

    let accounts = state
        .metadata
        .find_by_cosigner_commitment(&normalized_commitment)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to look up accounts by cosigner commitment");
            GuardianError::StorageError(format!(
                "Failed to look up accounts by cosigner commitment: {e}"
            ))
        })?;

    tracing::info!(
        key_commitment = %normalized_commitment,
        match_count = accounts.len(),
        "Lookup completed"
    );

    Ok(LookupAccountResult { accounts })
}

/// Normalize a `0x`-prefixed (or unprefixed) hex commitment to the canonical
/// `0x`-prefixed lowercase form used in `account_metadata.auth.cosigner_commitments`.
/// Rejects values that aren't exactly 32 bytes or contain non-hex characters.
fn normalize_commitment_hex(hex: &str) -> Result<String> {
    let trimmed = hex.trim_start_matches("0x").trim_start_matches("0X");
    if trimmed.len() != COMMITMENT_HEX_CHARS {
        return Err(GuardianError::InvalidInput(format!(
            "key_commitment must be 32 bytes ({COMMITMENT_HEX_CHARS} hex chars), got {} chars",
            trimmed.len()
        )));
    }
    let lower = trimmed.to_ascii_lowercase();
    ::hex::decode(&lower)
        .map_err(|e| GuardianError::InvalidInput(format!("invalid key_commitment hex: {e}")))?;
    Ok(format!("0x{lower}"))
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;

    #[test]
    fn normalize_commitment_hex_accepts_lowercase_prefixed() {
        let input = format!("0x{}", "ab".repeat(32));
        let out = normalize_commitment_hex(&input).expect("should normalize");
        assert_eq!(out, input);
    }

    #[test]
    fn normalize_commitment_hex_lowercases_uppercase_input() {
        let input = format!("0x{}", "AB".repeat(32));
        let expected = format!("0x{}", "ab".repeat(32));
        let out = normalize_commitment_hex(&input).expect("should normalize");
        assert_eq!(out, expected);
    }

    #[test]
    fn normalize_commitment_hex_accepts_unprefixed() {
        let input = "ab".repeat(32);
        let out = normalize_commitment_hex(&input).expect("should normalize");
        assert_eq!(out, format!("0x{input}"));
    }

    #[test]
    fn normalize_commitment_hex_rejects_short() {
        let err = normalize_commitment_hex("0xabcd").expect_err("must reject");
        match err {
            GuardianError::InvalidInput(msg) => {
                assert!(msg.contains("32 bytes"), "{msg}");
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn normalize_commitment_hex_rejects_long() {
        let input = format!("0x{}", "ab".repeat(33));
        let err = normalize_commitment_hex(&input).expect_err("must reject");
        match err {
            GuardianError::InvalidInput(_) => {}
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn normalize_commitment_hex_rejects_non_hex() {
        let input = format!("0x{}", "zz".repeat(32));
        let err = normalize_commitment_hex(&input).expect_err("must reject");
        match err {
            GuardianError::InvalidInput(_) => {}
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }
}
