use guardian_shared::session_grant::SessionLogoutMessage;

use crate::builder::state::AppState;
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;

/// Session-key-signed logout (`POST /session/logout`).
#[derive(Debug, Clone)]
pub struct RevokeSessionParams {
    /// `x-pubkey` is the session public key; `x-signature` signs
    /// `SessionLogoutMessage(x-pubkey, x-timestamp)`.
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct RevokeSessionResult {
    /// Whether an active session existed. Logout is idempotent: revoking an
    /// already revoked or expired session succeeds with `false`.
    pub revoked: bool,
}

#[tracing::instrument(level = "info", skip(state, params))]
pub async fn revoke_session(
    state: &AppState,
    params: RevokeSessionParams,
) -> Result<RevokeSessionResult> {
    let (pubkey_hex, signature_hex, timestamp) =
        params.credentials.as_signature().ok_or_else(|| {
            GuardianError::AuthenticationFailed("missing signature credentials".into())
        })?;
    super::validate_timestamp_skew(state, timestamp)?;

    let session_public_key = crate::session::credential_public_key(pubkey_hex)?;
    let message = SessionLogoutMessage::new(&session_public_key, timestamp).to_word();
    crate::session::verify_signature(&session_public_key, signature_hex, message)?;

    let signer_commitment = state.miden_sessions.revoke(&session_public_key).await?;
    tracing::info!(
        revoked = signer_commitment.is_some(),
        signer_commitment = signer_commitment.as_deref().unwrap_or_default(),
        "Session logout"
    );
    Ok(RevokeSessionResult {
        revoked: signer_commitment.is_some(),
    })
}
