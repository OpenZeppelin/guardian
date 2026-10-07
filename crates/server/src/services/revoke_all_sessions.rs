use guardian_shared::auth_request_eip712::revoke_all_digest;
use guardian_shared::hex::FromHex;
use guardian_shared::session_grant::SessionRevokeAllMessage;
use miden_protocol::Word;

use crate::builder::state::AppState;
use crate::error::{GuardianError, Result};
use crate::metadata::auth::lookup::{
    commitment_of, derive_pubkey_from_raw_signature, is_ecdsa_signature_hex,
    verify_eip712_signature, verify_raw_ecdsa_signature,
};
use crate::metadata::auth::{Credentials, RequestAuthFormat};

/// Wallet-signed revocation of every session of a signer
/// (`POST /session/revoke-all`) issued at or before the signed timestamp.
#[derive(Debug, Clone)]
pub struct RevokeAllSessionsParams {
    /// Commitment of the wallet key whose sessions end; it must be the key
    /// that signs the request.
    pub signer_commitment: String,
    /// `x-signature` signs `SessionRevokeAllMessage(signer_commitment,
    /// x-timestamp)`: its RPO digest (raw) or its EIP-712 typed data
    /// (`x-auth-format: eip712`).
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct RevokeAllSessionsResult {
    /// How many active sessions were revoked; zero when none existed, so the
    /// call is idempotent.
    pub revoked: u64,
}

#[tracing::instrument(
    level = "info",
    skip(state, params),
    fields(signer_commitment = tracing::field::Empty)
)]
pub async fn revoke_all_sessions(
    state: &AppState,
    params: RevokeAllSessionsParams,
) -> Result<RevokeAllSessionsResult> {
    let signer_commitment = Word::from_hex(&params.signer_commitment)
        .map_err(|e| GuardianError::InvalidInput(format!("invalid signer_commitment: {e}")))?;

    // Account-less: it never touches a replay floor, so no session request
    // can block it. A replay inside the clock-skew window only ends sessions
    // issued at or before the signed timestamp, which were already ended.
    let timestamp = params.credentials.timestamp();
    super::validate_timestamp_skew(state, timestamp)?;
    let message = SessionRevokeAllMessage::new(signer_commitment, timestamp);

    let (pubkey_hex, signature_hex, _) = params.credentials.as_signature().ok_or_else(|| {
        GuardianError::AuthenticationFailed("missing signature credentials".into())
    })?;

    let verified_key = match params.credentials.auth_format() {
        // Recover the key (Falcon embeds it). An ECDSA signature that does
        // not recover to the signer's key falls back to the supplied key, the
        // same rule grant registration and request auth use.
        RequestAuthFormat::Raw => {
            match derive_pubkey_from_raw_signature(signature_hex, message.to_word()) {
                Ok(key) if commitment_of(&key).eq_ignore_ascii_case(&params.signer_commitment) => {
                    Ok(key)
                }
                _ if is_ecdsa_signature_hex(signature_hex) => verify_raw_ecdsa_signature(
                    "revoke-all",
                    signature_hex,
                    pubkey_hex,
                    message.to_word(),
                ),
                derived => derived,
            }
        }
        RequestAuthFormat::Eip712 => verify_eip712_signature(
            "revoke-all",
            signature_hex,
            pubkey_hex,
            revoke_all_digest(&message),
        ),
        // Revocation is the recovery path for a stolen session key:
        // wallet-only (#219).
        RequestAuthFormat::Session => {
            return Err(super::reject_session_credentials(
                state,
                &params.credentials,
                message.to_word(),
            )
            .await);
        }
    }
    .map_err(GuardianError::AuthenticationFailed)?;
    let verified_commitment = commitment_of(&verified_key);
    if !verified_commitment.eq_ignore_ascii_case(&params.signer_commitment) {
        return Err(GuardianError::AuthenticationFailed(
            "signature key does not match signer_commitment".into(),
        ));
    }
    tracing::Span::current().record(
        "signer_commitment",
        tracing::field::display(&verified_commitment),
    );

    let issued_at_or_before = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp)
        .ok_or_else(|| GuardianError::InvalidInput("invalid timestamp".to_string()))?;
    let revoked = state
        .miden_sessions
        .revoke_all(&verified_commitment, issued_at_or_before, state.clock.now())
        .await?;
    tracing::info!(revoked, "Sessions revoked by wallet");
    Ok(RevokeAllSessionsResult { revoked })
}
