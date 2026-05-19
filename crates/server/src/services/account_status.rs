//! Per-account pause chokepoint (feature 001-account-pausing).
//!
//! Single helper [`ensure_account_active`] consulted from every
//! per-account mutating entry point (multisig + EVM proposal pipelines).
//! Admin/setup paths (`services::configure_account`,
//! `evm::service::register_account`) deliberately do NOT call this
//! helper — see spec Non-Goals.
//!
//! FR-025 single-call-site invariant: this module is the ONLY place
//! outside read endpoints + the pause/unpause handlers that reads
//! `AccountMetadata::paused_at`. When the broader `PolicyEngine` (#182)
//! lands, this helper is replaced wholesale by `policy_engine.evaluate_all(...)`
//! with no API, audit, or storage change (FR-026 / SC-007).
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{GuardianError, Result};
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountStatus {
    Active,
    Paused,
}

impl AccountStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
        }
    }
}

/// Outcome of a pause/unpause transition. The `before_state` /
/// `after_state` pair encodes idempotent retries (FR-019): a re-pause
/// of an already-paused account produces `(Paused, Paused)` with the
/// original `paused_at` preserved.
#[derive(Debug, Clone)]
pub struct PauseTransition {
    pub before_state: AccountStatus,
    pub after_state: AccountStatus,
    pub paused_at: Option<DateTime<Utc>>,
    pub paused_reason: Option<String>,
}

/// Returns `Ok(())` if the account is active and may proceed, or
/// `GuardianError::AccountPaused { .. }` carrying the persisted
/// `paused_at` / `paused_reason` when the account is paused. Missing
/// account surfaces as `GuardianError::AccountNotFound` to keep the
/// existing not-found error model unchanged on the mutating paths.
pub async fn ensure_account_active(state: &AppState, account_id: &str) -> Result<()> {
    let metadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| GuardianError::StorageError(format!("Failed to load metadata: {e}")))?
        .ok_or_else(|| GuardianError::AccountNotFound(account_id.to_string()))?;

    if let Some(paused_at) = metadata.paused_at {
        return Err(GuardianError::AccountPaused {
            paused_at,
            paused_reason: metadata.paused_reason,
        });
    }
    Ok(())
}
