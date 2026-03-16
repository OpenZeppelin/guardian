//! Shared helpers for decoding account state payloads from PSM.

use base64::Engine;
use miden_client::Deserializable;
use miden_client::account::Account;

use crate::error::{MultisigError, Result};

/// Decodes a full `Account` from PSM `state_json` payload.
pub(crate) fn decode_account_from_state_json(state_json: &str) -> Result<Account> {
    let state_value: serde_json::Value = serde_json::from_str(state_json)?;
    let account_base64 = state_value["data"]
        .as_str()
        .ok_or_else(|| MultisigError::PsmServer("missing 'data' field in state".to_string()))?;
    decode_account_from_base64(account_base64)
}

/// Decodes a full `Account` from base64 payload.
pub(crate) fn decode_account_from_base64(account_base64: &str) -> Result<Account> {
    let account_bytes = base64::engine::general_purpose::STANDARD
        .decode(account_base64)
        .map_err(|e| MultisigError::MidenClient(format!("failed to decode account: {}", e)))?;

    Account::read_from_bytes(&account_bytes)
        .map_err(|e| MultisigError::MidenClient(format!("failed to deserialize account: {}", e)))
}

/// Returns true if an error indicates the local store has stale/locked account commitment state.
pub(crate) fn is_commitment_mismatch_error(error: &MultisigError) -> bool {
    error
        .to_string()
        .contains("doesn't match the imported account commitment")
}
