use chrono::{DateTime, Utc};
use guardian_shared::SignatureScheme;
use guardian_shared::hex::FromHex;
use guardian_shared::session_grant::SessionGrant;
use miden_protocol::Word;

use crate::builder::state::AppState;
use crate::error::{GuardianError, Result};
use crate::metadata::auth::{MAX_TIMESTAMP_SKEW_SECS, RequestAuthFormat, verify_session_grant};
use crate::session::MidenSession;

/// A wallet-signed session grant (`POST /session`).
#[derive(Debug, Clone)]
pub struct CreateSessionParams {
    pub scheme: SignatureScheme,
    /// `Raw` or `Eip712`; `Eip712` is ECDSA-only.
    pub auth_format: RequestAuthFormat,
    /// Wallet public key; required for ECDSA, ignored for Falcon (the key is
    /// embedded in the signature).
    pub public_key: Option<String>,
    pub signature: String,
    pub signer_commitment: String,
    pub session_public_key: String,
    /// The website that asked for the grant, shown by the wallet; empty for
    /// clients outside a browser. Informational: Guardian does not check it
    /// against requests.
    pub origin: String,
    /// Unix seconds.
    pub issued_at: u64,
    /// Unix seconds.
    pub expires_at: u64,
    pub guardian_commitment: String,
    pub network: String,
}

#[derive(Debug, Clone)]
pub struct CreateSessionResult {
    pub signer_commitment: String,
    pub expires_at: DateTime<Utc>,
}

/// Registers a delegated signer after checking the grant against this
/// Guardian and verifying the wallet signature.
///
/// The endpoint is unauthenticated until the signature is verified, so every
/// check that needs no cryptography runs first and malformed grants are
/// rejected before any signature verification. Grant times are Unix seconds,
/// so the skew window is compared in seconds. Only a cosigner of some account
/// may hold sessions, so arbitrary keys cannot fill the store; that check
/// uses the lookup index and runs after the signature verifies.
#[tracing::instrument(
    level = "info",
    skip(state, params),
    fields(scheme = %params.scheme, auth_format = ?params.auth_format)
)]
pub async fn create_session(
    state: &AppState,
    params: CreateSessionParams,
) -> Result<CreateSessionResult> {
    let rejected = |category: &'static str, error: GuardianError| {
        tracing::warn!(category, %error, "Session grant rejected");
        error
    };

    super::check_wallet_signature_hex(&params.signature).map_err(|e| rejected("signature", e))?;
    let network = state.dashboard.environment();
    if params.network != network {
        return Err(rejected(
            "network",
            GuardianError::InvalidInput(format!(
                "Session grant is for network '{}', expected '{network}'",
                params.network
            )),
        ));
    }
    let guardian_commitment = state.ack.commitment(&params.scheme);
    if !params
        .guardian_commitment
        .eq_ignore_ascii_case(&guardian_commitment)
    {
        return Err(rejected(
            "guardian_key",
            GuardianError::InvalidInput(
                "Session grant is bound to a different Guardian key".to_string(),
            ),
        ));
    }

    let now = state.clock.now();
    let now_secs = now.timestamp().max(0) as u64;
    if params.issued_at.abs_diff(now_secs) > MAX_TIMESTAMP_SKEW_SECS {
        return Err(rejected(
            "issued_at",
            GuardianError::AuthenticationFailed(format!(
                "Session grant issued_at is more than {MAX_TIMESTAMP_SKEW_SECS}s from server time"
            )),
        ));
    }
    let remaining = params.expires_at.saturating_sub(now_secs);
    if remaining <= MAX_TIMESTAMP_SKEW_SECS {
        return Err(rejected(
            "lifetime",
            GuardianError::InvalidInput(format!(
                "Session grant must expire more than {MAX_TIMESTAMP_SKEW_SECS}s from now"
            )),
        ));
    }
    let max_ttl = state.miden_sessions.config().max_ttl_seconds();
    if remaining > max_ttl {
        return Err(rejected(
            "lifetime",
            GuardianError::InvalidInput(format!(
                "Session grant expires more than {max_ttl}s from now"
            )),
        ));
    }

    let session_public_key = crate::session::parse_public_key_hex(&params.session_public_key)
        .map_err(|e| rejected("session_key", e))?;
    let signer_word = parse_word(&params.signer_commitment, "signer commitment")
        .map_err(|e| rejected("grant", e))?;
    let guardian_word = parse_word(&guardian_commitment, "Guardian commitment")?;
    let grant = SessionGrant::new(
        signer_word,
        &session_public_key,
        params.origin.as_str(),
        params.issued_at,
        params.expires_at,
        guardian_word,
        network,
    )
    .map_err(|e| rejected("origin", GuardianError::InvalidInput(e)))?;

    let signer_commitment = verify_session_grant(
        params.scheme,
        params.auth_format,
        &grant,
        &params.signature,
        params.public_key.as_deref(),
    )
    .map_err(|e| rejected("signature", GuardianError::AuthenticationFailed(e)))?;
    if !signer_commitment.eq_ignore_ascii_case(&params.signer_commitment) {
        return Err(rejected(
            "signer",
            GuardianError::AuthenticationFailed(
                "Session grant signer commitment does not match the signing key".to_string(),
            ),
        ));
    }

    let cosigned = state
        .metadata
        .find_by_cosigner_commitment(&signer_commitment.to_ascii_lowercase())
        .await
        .map_err(|e| GuardianError::StorageError(format!("Failed to look up signer: {e}")))?;
    if cosigned.is_empty() {
        return Err(rejected(
            "not_a_cosigner",
            GuardianError::AuthorizationFailed(
                "Session grant signer is not a cosigner of any account".to_string(),
            ),
        ));
    }

    let expires_at = state
        .miden_sessions
        .register(
            &session_public_key,
            MidenSession {
                signer_commitment: signer_commitment.clone(),
                origin: params.origin,
                guardian_commitment,
                network: network.to_string(),
            },
            params.issued_at,
            params.expires_at,
            now,
        )
        .await
        .map_err(|e| rejected("session_key", e))?;
    tracing::info!(
        signer_commitment = %signer_commitment,
        expires_at = %expires_at,
        "Session registered"
    );

    Ok(CreateSessionResult {
        signer_commitment,
        expires_at,
    })
}

fn parse_word(value: &str, label: &str) -> Result<Word> {
    Word::from_hex(value).map_err(|e| GuardianError::InvalidInput(format!("Invalid {label}: {e}")))
}
