use crate::coordination::read_records::{HeadRead, PrefetchedHead, ReadRule};
use crate::error::{GuardianError, Result};
use crate::metadata::SessionFloorOutcome;
use crate::metadata::auth::{
    Credentials, MAX_TIMESTAMP_SKEW_MS, RequestAuthFormat, SESSION_NONCE_FUTURE_MS,
    SESSION_READ_SKEW_MS, TimestampWindow,
};
use crate::metadata::{AccountMetadata, LEGACY_ACCOUNT_AUTH_FLOOR};
use crate::metrics::labels::{
    AuthCredential, AuthRejectionReason, record_auth_rejection, record_session_read,
};
use crate::state::AppState;
use crate::storage::StorageBackend;
use base64::Engine;
use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::session_request::{ReplayPolicy, SessionOperation, SessionRequestEnvelope};
use serde_json::Value;
use std::sync::Arc;

mod abandon_candidate;
pub mod account_status;
pub(crate) mod ack_delta_internal;
pub mod candidate_chain;
mod configure_account;
mod create_session;
mod dashboard_account_delta_detail;
mod dashboard_account_deltas;
mod dashboard_account_proposals;
mod dashboard_account_snapshot;
mod dashboard_accounts;
mod dashboard_global_deltas;
mod dashboard_global_proposals;
mod dashboard_info;
mod dashboard_pagination;
mod dashboard_stats;
mod delta_commit;
pub mod execute_proposal;
pub mod execution_codec;
pub mod execution_status;
mod get_canonical_nonce;
mod get_delta;
mod get_delta_history;
mod get_delta_proposal;
mod get_delta_proposals;
mod get_delta_since;
mod get_state;
mod get_state_tail;
mod lookup_account;
mod multisig_admission;
pub mod pause_account;
pub(crate) mod proposal_signature;
mod push_delta;
mod push_delta_proposal;
pub mod release_on_switch;
mod revoke_all_sessions;
mod revoke_session;
mod sign_delta_proposal;
mod status;
pub mod unpause_account;

pub use abandon_candidate::{
    AbandonCandidateParams, AbandonCandidateResult, AbandonState, abandon_candidate,
};

pub use crate::jobs::canonicalization::{
    process_canonicalizations_now, start_canonicalization_worker,
};
pub use configure_account::{ConfigureAccountParams, ConfigureAccountResult, configure_account};
pub use create_session::{CreateSessionParams, CreateSessionResult, create_session};
pub use dashboard_account_delta_detail::{
    DashboardDeltaDetail, DetailIncludeFlags, get_account_delta_detail,
};
pub use dashboard_account_deltas::{
    DashboardDeltaEntry, DashboardDeltaStatus, list_account_deltas,
};
pub use dashboard_account_proposals::{DashboardProposalEntry, list_account_proposals};
pub use dashboard_account_snapshot::{
    DashboardAccountSnapshot, DashboardVaultFungibleEntry, DashboardVaultNonFungibleEntry,
    DashboardVaultSnapshot, get_account_snapshot,
};
pub use dashboard_accounts::{
    DashboardAccountDetail, DashboardAccountStateStatus, DashboardAccountSummary,
    GetDashboardAccountResult, get_dashboard_account, list_dashboard_accounts_paged,
    normalized_authorized_signer_count,
};
pub use dashboard_global_deltas::{
    DashboardGlobalDeltaEntry, list_global_deltas, parse_status_filter,
};
pub use dashboard_global_proposals::{DashboardGlobalProposalEntry, list_global_proposals};
pub use dashboard_info::{
    AGG_ACCOUNTS_BY_AUTH_METHOD, AGG_DELTA_STATUS_COUNTS, AGG_IN_FLIGHT_PROPOSAL_COUNT,
    AGG_LATEST_ACTIVITY, DashboardDeltaStatusCounts, DashboardInfoResponse, DashboardServiceStatus,
    get_dashboard_info,
};
pub use dashboard_pagination::{
    DEFAULT_LIMIT, MAX_LIMIT, PagedResult, parse_cursor, parse_limit, validate_limit,
};
pub use dashboard_stats::{
    DashboardAccountStats, DashboardAssetStats, DashboardAuthMethodSignerCount,
    DashboardFungibleTotal, DashboardLifecycleCounts, DashboardNonFungibleTotal,
    DashboardStatsRefreshResponse, DashboardStatsRefreshStatus, DashboardStatsResponse,
    get_dashboard_stats, parse_updated_since, request_dashboard_stats_refresh,
};
pub use get_canonical_nonce::{
    CanonicalNonceResponse, GetCanonicalNonceParams, get_canonical_nonce,
};
pub use get_delta::{GetDeltaParams, GetDeltaResult, get_delta};
pub use get_delta_history::{
    GetDeltaHistoryParams, HistoryEntry, HistoryEntryStatus, get_delta_history,
};
pub use get_delta_proposal::{GetDeltaProposalParams, GetDeltaProposalResult, get_delta_proposal};
pub use get_delta_proposals::{
    GetDeltaProposalsParams, GetDeltaProposalsResult, get_delta_proposals,
};
pub use get_delta_since::{GetDeltaSinceParams, GetDeltaSinceResult, get_delta_since};
pub use get_state::{GetStateParams, GetStateResult, get_state};
pub use get_state_tail::{GetStateTailParams, QueuedCandidate, StateTail, get_state_tail};
pub use lookup_account::{LookupAccountParams, LookupAccountResult, lookup_account};
pub use push_delta::{PushDeltaParams, PushDeltaResult, push_delta};
pub use push_delta_proposal::{
    PushDeltaProposalParams, PushDeltaProposalResult, push_delta_proposal,
};
pub use revoke_all_sessions::{
    RevokeAllSessionsParams, RevokeAllSessionsResult, revoke_all_sessions,
};
pub use revoke_session::{RevokeSessionParams, RevokeSessionResult, revoke_session};
pub use sign_delta_proposal::{
    SignDeltaProposalParams, SignDeltaProposalResult, sign_delta_proposal,
};
pub use status::{ExecutionStatus, SessionsStatus, StatusResponse, build_status};

#[derive(Clone)]
pub struct ResolvedAccount {
    pub metadata: AccountMetadata,
    pub storage: Arc<dyn StorageBackend>,
    /// Identity verified by the request authenticator.
    pub signer_commitment: String,
}

impl std::fmt::Debug for ResolvedAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedAccount")
            .field("metadata", &self.metadata)
            .field("storage", &"<StorageBackend>")
            .finish()
    }
}

/// Resolves an account and authenticates a request on a wallet-only route.
///
/// Session (delegated-signer) credentials are denied by default: a route
/// accepts them only by calling [`resolve_account_allowing_session`]. Routes
/// added later, and gRPC methods without an HTTP twin, are wallet-only until
/// they opt in.
pub async fn resolve_account(
    state: &AppState,
    account_id: &str,
    creds: &Credentials,
) -> Result<ResolvedAccount> {
    resolve_account_with(
        state,
        account_id,
        creds,
        RouteAccess::WalletOnly,
        MetadataRead::Authoritative,
    )
    .await
}

/// Resolves an account and authenticates a request on a session-eligible
/// route: reads, proposal list/get, proposal create/sign, delta pushes,
/// execution requests and status reads (FR-008 of issue #219). The wallet may always sign these too.
pub async fn resolve_account_allowing_session(
    state: &AppState,
    account_id: &str,
    creds: &Credentials,
    operation: SessionOperation,
) -> Result<ResolvedAccount> {
    resolve_account_with(
        state,
        account_id,
        creds,
        RouteAccess::Session(operation),
        MetadataRead::Authoritative,
    )
    .await
}

/// [`resolve_account_allowing_session`] for read routes: the account's
/// metadata may come from a cached copy at or above the version the database
/// reported after the request's last wait (a session read re-reads it with
/// the session), so a signer removal committed before that point refuses the
/// request and one committed after it is honored on the next request. Routes
/// that write must use [`resolve_account_allowing_session`].
pub async fn resolve_account_for_read(
    state: &AppState,
    account_id: &str,
    creds: &Credentials,
    operation: SessionOperation,
) -> Result<ResolvedAccount> {
    debug_assert_eq!(operation.replay_policy(), ReplayPolicy::Read);
    resolve_account_with(
        state,
        account_id,
        creds,
        RouteAccess::Session(operation),
        MetadataRead::ValidatedCopy,
    )
    .await
}

/// [`resolve_account_for_read`] for the canonical nonce poll: a session read
/// admitted in one database call also brings the canonical state head.
pub async fn resolve_account_for_head_read(
    state: &AppState,
    account_id: &str,
    creds: &Credentials,
) -> Result<(ResolvedAccount, PrefetchedHead)> {
    resolve_account_with_head(
        state,
        account_id,
        creds,
        RouteAccess::Session(SessionOperation::GetCanonicalNonce),
        MetadataRead::ValidatedCopy,
        HeadRead::Include,
    )
    .await
}

/// A session read: the window and the signature over the request's envelope
/// are checked without the database, then one call looks up the account's
/// metadata version and the session and admits the read by its rule
/// ([`SessionReadKind`]): once, or every time inside its window. The metadata comes from the store's
/// copy at that version, and the grant is checked as for every session
/// request. A read admitted before an ownership check refuses it stays recorded; it
/// was not a request its signer could use anyway.
async fn resolve_unique_session_read(
    state: &AppState,
    account_id: &str,
    creds: &Credentials,
    operation: SessionOperation,
    head: HeadRead,
) -> Result<(ResolvedAccount, PrefetchedHead)> {
    use crate::coordination::read_records::{
        InactiveSession, SessionReadAdmission, SessionReadRequest,
    };

    let kind = SessionReadKind::of(operation);
    let request_timestamp = validate_request_timestamp_in(state, account_id, creds, kind.window())
        .inspect_err(|_| {
            record_auth_rejection(
                AuthCredential::Session,
                AuthRejectionReason::TimestampWindow,
            )
        })?;
    let (pubkey_hex, signature_hex, _) = creds.as_signature().ok_or_else(|| {
        GuardianError::AuthenticationFailed("Session requests require signature credentials".into())
    })?;
    let session_public_key = crate::session::credential_public_key(pubkey_hex)?;
    let message = AuthRequestMessage::from_account_id_hex(
        account_id,
        request_timestamp,
        creds.request_payload().clone(),
    )
    .map_err(GuardianError::AuthenticationFailed)?;
    let envelope = SessionRequestEnvelope::new(creds.transport(), operation, &message);
    crate::session::verify_signature_bytes(
        &session_public_key,
        signature_hex,
        &envelope.to_bytes(),
    )
    .inspect_err(|_| {
        record_auth_rejection(
            AuthCredential::Session,
            AuthRejectionReason::InvalidSignature,
        )
    })?;

    let rule = kind.rule(&envelope, &session_public_key);
    let admission = state
        .miden_sessions
        .read_records()
        .admit_session_read(
            &StateSessionReadLookups { state },
            SessionReadRequest {
                account_id,
                session_key: crate::session::store_key(&session_public_key),
                rule,
                timestamp_ms: request_timestamp,
                clock: state.clock.as_ref(),
                head,
            },
        )
        .await
        .map_err(|e| read_record_error(account_id, e))?;
    let (metadata_version, subject, outcome, prefetched_head) = match admission {
        SessionReadAdmission::AccountNotFound => {
            return Err(GuardianError::AccountNotFound(account_id.to_string()));
        }
        SessionReadAdmission::Inactive(status) => {
            record_auth_rejection(AuthCredential::Session, AuthRejectionReason::SessionInvalid);
            return Err(match status {
                InactiveSession::Revoked => GuardianError::SessionRevoked,
                InactiveSession::Expired => GuardianError::SessionExpired,
                InactiveSession::Unknown => {
                    GuardianError::AuthenticationFailed("Unknown session key".to_string())
                }
            });
        }
        SessionReadAdmission::Live {
            metadata_version,
            subject,
            outcome,
            head,
        } => (metadata_version, subject, outcome, head),
    };
    let session = crate::session::MidenSession::from_subject(subject).ok_or_else(|| {
        record_auth_rejection(AuthCredential::Session, AuthRejectionReason::SessionInvalid);
        GuardianError::AuthenticationFailed("Unknown session key".to_string())
    })?;

    let metadata = match metadata_version {
        Some(version) => state.metadata.get_for_read_at(account_id, version).await,
        None => state.metadata.get_for_read(account_id).await,
    }
    .map_err(|e| GuardianError::StorageError(format!("Failed to check account: {e}")))?
    .ok_or_else(|| GuardianError::AccountNotFound(account_id.to_string()))?;
    if metadata.network_config.is_evm()
        || matches!(metadata.auth, crate::metadata::Auth::EvmEcdsa { .. })
    {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "delta_api".to_string(),
        });
    }
    let signer_commitment = check_session_grant(state, account_id, &metadata, session)?;
    admit_read_record(state, account_id, request_timestamp, rule, outcome)?;
    Ok((
        ResolvedAccount {
            metadata,
            storage: state.storage.clone(),
            signer_commitment,
        },
        prefetched_head,
    ))
}

/// How a session read is admitted. The canonical nonce poll is accepted every
/// time it is presented inside a window of [`SESSION_READ_SKEW_MS`] behind and
/// [`SESSION_NONCE_FUTURE_MS`] ahead of server time: a copy reveals only when
/// the account changes, and recording every poll would make the most frequent
/// request a durable write. Every other session read is admitted once,
/// fleet-wide, inside [`SESSION_READ_SKEW_MS`] either side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionReadKind {
    Recorded,
    WindowOnly,
}

impl SessionReadKind {
    fn of(operation: SessionOperation) -> Self {
        match operation {
            SessionOperation::GetCanonicalNonce => Self::WindowOnly,
            SessionOperation::GetState
            | SessionOperation::GetStateTail
            | SessionOperation::GetDelta
            | SessionOperation::GetDeltaSince
            | SessionOperation::GetDeltaHistory
            | SessionOperation::GetDeltaProposals
            | SessionOperation::GetDeltaProposal
            | SessionOperation::GetDeltaProposalExecution
            | SessionOperation::GetCurrentExecution
            | SessionOperation::PushDelta
            | SessionOperation::PushDeltaProposal
            | SessionOperation::SignDeltaProposal
            | SessionOperation::ExecuteDeltaProposal => Self::Recorded,
        }
    }

    fn window(self) -> TimestampWindow {
        match self {
            Self::Recorded => TimestampWindow::symmetric(SESSION_READ_SKEW_MS),
            Self::WindowOnly => TimestampWindow {
                past_ms: SESSION_READ_SKEW_MS,
                future_ms: SESSION_NONCE_FUTURE_MS,
            },
        }
    }

    fn rule(self, envelope: &SessionRequestEnvelope, session_public_key: &[u8]) -> ReadRule {
        match self {
            Self::Recorded => ReadRule::Record {
                key: envelope.record_key(session_public_key),
                skew_ms: SESSION_READ_SKEW_MS,
            },
            Self::WindowOnly => ReadRule::WindowOnly(self.window()),
        }
    }
}

fn read_record_error(account_id: &str, error: String) -> GuardianError {
    tracing::error!(account_id = %account_id, error = %error, "Failed to admit session read");
    GuardianError::StorageError(format!("Failed to admit session read: {error}"))
}

/// Accepts a session read its rule admitted. A copy of one already
/// admitted, or one at or below the fence, is a replay; one that left its
/// window while it waited is `timestamp_out_of_window`.
fn admit_read_record(
    state: &AppState,
    account_id: &str,
    request_timestamp: i64,
    rule: ReadRule,
    outcome: crate::coordination::ReadRecordOutcome,
) -> Result<()> {
    use crate::coordination::ReadRecordOutcome;
    record_session_read(rule.kind(), outcome.as_str());
    let reason = match outcome {
        ReadRecordOutcome::Recorded | ReadRecordOutcome::Served => return Ok(()),
        ReadRecordOutcome::OutsideWindow => {
            record_auth_rejection(
                AuthCredential::Session,
                AuthRejectionReason::TimestampWindow,
            );
            return Err(late_request(state, request_timestamp, rule.window()));
        }
        ReadRecordOutcome::Duplicate => AuthRejectionReason::Replay,
        ReadRecordOutcome::Stale => AuthRejectionReason::Stale,
    };
    record_auth_rejection(AuthCredential::Session, reason);
    tracing::warn!(
        account_id = %account_id,
        request_timestamp = %request_timestamp,
        outcome = ?outcome,
        "Replay rejected: session read already admitted or outside its window at admission"
    );
    Err(GuardianError::AuthenticationReplay)
}

/// State head, account and session lookups for stores that admit session
/// reads locally.
struct StateSessionReadLookups<'a> {
    state: &'a AppState,
}

#[async_trait::async_trait]
impl crate::coordination::read_records::SessionReadLookups for StateSessionReadLookups<'_> {
    async fn head(
        &self,
        account_id: &str,
    ) -> std::result::Result<Option<crate::state_object::StateHead>, String> {
        match self.state.storage.pull_state_head(account_id).await {
            Ok(head) => Ok(Some(head)),
            Err(error) if crate::storage::is_storage_not_found(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn account_exists(&self, account_id: &str) -> std::result::Result<bool, String> {
        Ok(self
            .state
            .metadata
            .get_for_read(account_id)
            .await?
            .is_some())
    }

    async fn session(
        &self,
        key: &crate::coordination::SessionKey,
    ) -> std::result::Result<crate::coordination::read_records::SessionStatus, String> {
        self.state
            .miden_sessions
            .status(key, self.state.clock.now())
            .await
            .map_err(|error| error.to_string())
    }
}

/// Whether a per-account route accepts session credentials, and as which
/// signed operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RouteAccess {
    WalletOnly,
    Session(SessionOperation),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetadataRead {
    Authoritative,
    ValidatedCopy,
}

async fn resolve_account_with(
    state: &AppState,
    account_id: &str,
    creds: &Credentials,
    access: RouteAccess,
    metadata_read: MetadataRead,
) -> Result<ResolvedAccount> {
    resolve_account_with_head(
        state,
        account_id,
        creds,
        access,
        metadata_read,
        HeadRead::Skip,
    )
    .await
    .map(|(resolved, _)| resolved)
}

#[tracing::instrument(
    level = "debug",
    skip(state, creds),
    fields(account_id = %account_id)
)]
async fn resolve_account_with_head(
    state: &AppState,
    account_id: &str,
    creds: &Credentials,
    access: RouteAccess,
    metadata_read: MetadataRead,
    head: HeadRead,
) -> Result<(ResolvedAccount, PrefetchedHead)> {
    if creds.auth_format() == RequestAuthFormat::LegacySession {
        return Err(GuardianError::UnsupportedAuthVersion);
    }
    if creds.auth_format() == RequestAuthFormat::Session
        && let RouteAccess::Session(operation) = access
        && operation.replay_policy() == ReplayPolicy::Read
    {
        return resolve_unique_session_read(state, account_id, creds, operation, head).await;
    }

    let metadata = match metadata_read {
        MetadataRead::Authoritative => state.metadata.get(account_id).await,
        MetadataRead::ValidatedCopy => state.metadata.get_for_read(account_id).await,
    };
    let metadata = metadata
        .map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Failed to check account in resolve_account"
            );
            GuardianError::StorageError(format!("Failed to check account: {e}"))
        })?
        .ok_or_else(|| GuardianError::AccountNotFound(account_id.to_string()))?;

    let credential = if creds.auth_format() == RequestAuthFormat::Session {
        AuthCredential::Session
    } else {
        AuthCredential::Wallet
    };
    let window_ms = MAX_TIMESTAMP_SKEW_MS;
    let request_timestamp = validate_request_timestamp(state, account_id, creds, window_ms)
        .inspect_err(|_| record_auth_rejection(credential, AuthRejectionReason::TimestampWindow))?;

    if metadata.network_config.is_evm()
        || matches!(metadata.auth, crate::metadata::Auth::EvmEcdsa { .. })
    {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "delta_api".to_string(),
        });
    }

    let (signer_commitment, floor_key) = if creds.auth_format() == RequestAuthFormat::Session {
        let RouteAccess::Session(operation) = access else {
            let message = auth_request_word(account_id, creds)?;
            return Err(reject_session_credentials(state, creds, message).await);
        };
        let verified =
            verify_session_request(state, account_id, &metadata, creds, operation).await?;
        (
            verified.signer_commitment,
            crate::session::replay_floor_key(&verified.session_public_key),
        )
    } else {
        let signer_commitment = metadata.auth.verify(account_id, creds).map_err(|e| {
            record_auth_rejection(credential, AuthRejectionReason::InvalidSignature);
            tracing::warn!(
                account_id = %account_id,
                error = %e,
                "Authentication failed in resolve_account"
            );
            GuardianError::AuthenticationFailed(e)
        })?;
        (signer_commitment.clone(), signer_commitment)
    };

    consume_auth_timestamp(state, account_id, &floor_key, request_timestamp, window_ms).await?;

    let storage = state.storage.clone();

    Ok((
        ResolvedAccount {
            metadata,
            storage,
            signer_commitment,
        },
        PrefetchedHead::NotRead,
    ))
}

/// Authenticates delegated-signer credentials on a wallet-only route that
/// does not go through [`resolve_account`] (`/configure`, `/state/lookup`,
/// `/session/revoke-all`) and returns the error to reject them with:
/// `wallet_signature_required` once the delegated signature over `message`
/// verifies and the session is live, otherwise the authentication error.
/// Never returns success.
pub(crate) async fn reject_session_credentials(
    state: &AppState,
    creds: &Credentials,
    message: miden_protocol::Word,
) -> GuardianError {
    match authenticate_delegated_signer(state, creds, &message.as_bytes()).await {
        Ok(_) => GuardianError::WalletSignatureRequired,
        Err(error) => error,
    }
}

/// Verifies a delegated signer's signature over `message`, then returns its
/// session public key and live session, so a signature that does not verify
/// never reaches the session store. The grant's origin is shown to the user
/// by the wallet and is not checked against the request: a page that holds
/// the key can send its requests from anywhere.
async fn authenticate_delegated_signer(
    state: &AppState,
    creds: &Credentials,
    message: &[u8],
) -> Result<(
    [u8; guardian_shared::session_grant::SESSION_PUBLIC_KEY_LEN],
    crate::session::MidenSession,
)> {
    let (pubkey_hex, signature_hex, _) = creds.as_signature().ok_or_else(|| {
        GuardianError::AuthenticationFailed("Session requests require signature credentials".into())
    })?;
    let session_public_key = crate::session::credential_public_key(pubkey_hex)?;
    crate::session::verify_signature_bytes(&session_public_key, signature_hex, message)
        .inspect_err(|_| {
            record_auth_rejection(
                AuthCredential::Session,
                AuthRejectionReason::InvalidSignature,
            )
        })?;
    let session = state
        .miden_sessions
        .find(&session_public_key, state.clock.now())
        .await
        .inspect_err(|_| {
            record_auth_rejection(AuthCredential::Session, AuthRejectionReason::SessionInvalid)
        })?;
    Ok((session_public_key, session))
}

/// Authenticates a request signed by a delegated signer: verifies the P-256
/// signature over the version 2 envelope of this request (its transport, the
/// route's operation, and the `AuthRequestMessage` a wallet would sign),
/// resolves the session to its grant, re-checks that the grant still names
/// this Guardian and its network, and that its signer cosigns the account.
///
/// A signature made for another operation or transport does not verify, so
/// it never reaches the session store. A rotated ACK key or another network
/// ends every session granted before it (`authentication_failed`). A grant
/// for this Guardian's key of the other scheme, or a signer that does not
/// cosign the account, is `authorization_failed`: the session stays valid
/// for the signer's other accounts, and clients keep it.
async fn verify_session_request(
    state: &AppState,
    account_id: &str,
    metadata: &AccountMetadata,
    creds: &Credentials,
    operation: SessionOperation,
) -> Result<VerifiedSessionRequest> {
    let message = AuthRequestMessage::from_account_id_hex(
        account_id,
        creds.timestamp(),
        creds.request_payload().clone(),
    )
    .map_err(GuardianError::AuthenticationFailed)?;
    let envelope = SessionRequestEnvelope::new(creds.transport(), operation, &message);
    let (session_public_key, session) =
        authenticate_delegated_signer(state, creds, &envelope.to_bytes()).await?;
    let signer_commitment = check_session_grant(state, account_id, metadata, session)?;
    Ok(VerifiedSessionRequest {
        signer_commitment,
        session_public_key,
    })
}

/// A session request whose signature and grant check out.
struct VerifiedSessionRequest {
    signer_commitment: String,
    session_public_key: [u8; guardian_shared::session_grant::SESSION_PUBLIC_KEY_LEN],
}

/// Re-checks a live session's grant for this request: it must name this
/// Guardian's current ACK key and network, and its signer must cosign the
/// account under the scheme of that key. Returns the signer commitment.
fn check_session_grant(
    state: &AppState,
    account_id: &str,
    metadata: &AccountMetadata,
    session: crate::session::MidenSession,
) -> Result<String> {
    let names_ack_key = |scheme: &guardian_shared::SignatureScheme| {
        session
            .guardian_commitment
            .eq_ignore_ascii_case(&state.ack.commitment(scheme))
    };
    let names_this_guardian = [
        guardian_shared::SignatureScheme::Falcon,
        guardian_shared::SignatureScheme::Ecdsa,
    ]
    .iter()
    .any(names_ack_key);
    if !names_this_guardian || session.network != state.dashboard.environment() {
        tracing::warn!(
            account_id = %account_id,
            signer_commitment = %session.signer_commitment,
            "Session grant names another Guardian key or network"
        );
        record_auth_rejection(AuthCredential::Session, AuthRejectionReason::SessionInvalid);
        return Err(GuardianError::AuthenticationFailed(
            "Session grant names another Guardian key or network".to_string(),
        ));
    }
    // The grant names this Guardian's key of the wallet's scheme, which
    // must be the scheme the account registers that signer under.
    let signer_commitment = session.signer_commitment;
    let registered_scheme = metadata.auth.signer_scheme(&signer_commitment);
    if !registered_scheme.as_ref().is_some_and(names_ack_key) {
        tracing::warn!(
            account_id = %account_id,
            signer_commitment = %signer_commitment,
            "Session signer is not an authorized cosigner"
        );
        record_auth_rejection(AuthCredential::Session, AuthRejectionReason::NotCosigner);
        return Err(GuardianError::AuthorizationFailed(
            "Session signer is not an authorized cosigner of this account".to_string(),
        ));
    }
    Ok(signer_commitment)
}

/// The `AuthRequestMessage` word of a request. On a wallet-only route a
/// session key is checked against this word, so the request is refused as
/// `wallet_signature_required` only when its session is genuine and live.
pub(crate) fn auth_request_word(
    account_id: &str,
    creds: &Credentials,
) -> Result<miden_protocol::Word> {
    AuthRequestMessage::from_account_id_hex(
        account_id,
        creds.timestamp(),
        creds.request_payload().clone(),
    )
    .map(|message| message.to_word())
    .map_err(GuardianError::AuthenticationFailed)
}

/// Longest wallet signature a session route accepts: a Falcon signature with
/// its embedded public key.
const MAX_WALLET_SIGNATURE_BYTES: usize = crate::metadata::auth::FALCON_SIGNATURE_LEN;

/// Rejects a wallet signature on an unauthenticated session route
/// (`POST /session`, `POST /session/revoke-all`) unless it is even-length hex
/// of at most [`MAX_WALLET_SIGNATURE_BYTES`], before a verifier parses it.
pub(crate) fn check_wallet_signature_hex(signature: &str) -> Result<()> {
    let hex = signature.trim_start_matches("0x");
    if !hex.len().is_multiple_of(2)
        || hex.len() > 2 * MAX_WALLET_SIGNATURE_BYTES
        || !hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(GuardianError::InvalidInput(format!(
            "Signature must be hex of at most {MAX_WALLET_SIGNATURE_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Rejects a session-route request timestamp (ms) outside the allowed clock
/// skew window. Account requests use [`validate_request_timestamp`].
pub(crate) fn validate_timestamp_skew(state: &AppState, timestamp_ms: i64) -> Result<()> {
    let server_now_ms = state.clock.now().timestamp_millis();
    let time_diff_ms = server_now_ms.abs_diff(timestamp_ms);
    if time_diff_ms > MAX_TIMESTAMP_SKEW_MS as u64 {
        tracing::warn!(
            request_timestamp = %timestamp_ms,
            server_now_ms = %server_now_ms,
            time_diff_ms = %time_diff_ms,
            max_skew_ms = %MAX_TIMESTAMP_SKEW_MS,
            "Request timestamp outside allowed skew window"
        );
        return Err(GuardianError::TimestampOutOfWindow {
            drift_ms: time_diff_ms,
            window_ms: MAX_TIMESTAMP_SKEW_MS as u64,
        });
    }
    Ok(())
}

/// A request that was inside its window when it arrived but outside it by the
/// time its replay check ran: the same answer as a late arrival.
fn late_request(
    state: &AppState,
    request_timestamp: i64,
    window: TimestampWindow,
) -> GuardianError {
    let now_ms = state.clock.now().timestamp_millis();
    GuardianError::TimestampOutOfWindow {
        drift_ms: now_ms.abs_diff(request_timestamp),
        window_ms: window.bound_ms(request_timestamp, now_ms) as u64,
    }
}

/// Rejects an account request whose timestamp is more than `window_ms` from
/// server time: [`SESSION_READ_SKEW_MS`] for session reads,
/// [`MAX_TIMESTAMP_SKEW_MS`] for everything else.
pub(crate) fn validate_request_timestamp(
    state: &AppState,
    account_id: &str,
    creds: &Credentials,
    window_ms: i64,
) -> Result<i64> {
    validate_request_timestamp_in(
        state,
        account_id,
        creds,
        TimestampWindow::symmetric(window_ms),
    )
}

/// [`validate_request_timestamp`] for a window whose past and future sides
/// differ.
fn validate_request_timestamp_in(
    state: &AppState,
    account_id: &str,
    creds: &Credentials,
    window: TimestampWindow,
) -> Result<i64> {
    let request_timestamp = creds.timestamp();
    let server_now_ms = state.clock.now().timestamp_millis();
    let time_diff_ms = server_now_ms.abs_diff(request_timestamp);
    let window_ms = window.bound_ms(request_timestamp, server_now_ms);
    if !window.contains(request_timestamp, server_now_ms) {
        tracing::warn!(
            account_id = %account_id,
            request_timestamp = %request_timestamp,
            server_now_ms = %server_now_ms,
            time_diff_ms = %time_diff_ms,
            max_skew_ms = %window_ms,
            "Request timestamp outside allowed skew window"
        );
        return Err(GuardianError::TimestampOutOfWindow {
            drift_ms: time_diff_ms,
            window_ms: window_ms as u64,
        });
    }
    Ok(request_timestamp)
}

/// Advances the replay floor of (`account_id`, `signer_commitment`). For a
/// session request `signer_commitment` is the delegated key's floor key
/// (`session-…`), so a session never advances its wallet's floor; its floor
/// update re-checks the timestamp against `window_ms`, the request's window.
pub(crate) async fn consume_auth_timestamp(
    state: &AppState,
    account_id: &str,
    signer_commitment: &str,
    request_timestamp: i64,
    window_ms: i64,
) -> Result<()> {
    debug_assert_ne!(signer_commitment, LEGACY_ACCOUNT_AUTH_FLOOR);
    let storage_error = |e: String| {
        tracing::error!(
            account_id = %account_id,
            error = %e,
            "Failed to update last auth timestamp"
        );
        GuardianError::StorageError(format!("Failed to update last auth timestamp: {e}"))
    };
    let (credential, outcome) = if crate::session::is_replay_floor_key(signer_commitment) {
        let outcome = state
            .metadata
            .advance_session_floor(
                account_id,
                signer_commitment,
                request_timestamp,
                window_ms,
                state.clock.as_ref(),
            )
            .await
            .map_err(storage_error)?;
        (AuthCredential::Session, outcome)
    } else {
        let advanced = state
            .metadata
            .update_last_auth_timestamp_cas(account_id, signer_commitment, request_timestamp)
            .await
            .map_err(storage_error)?;
        let outcome = if advanced {
            SessionFloorOutcome::Advanced
        } else {
            SessionFloorOutcome::Replay
        };
        (AuthCredential::Wallet, outcome)
    };

    let reason = match outcome {
        SessionFloorOutcome::Advanced => return Ok(()),
        SessionFloorOutcome::OutsideWindow => {
            record_auth_rejection(credential, AuthRejectionReason::TimestampWindow);
            return Err(late_request(
                state,
                request_timestamp,
                TimestampWindow::symmetric(window_ms),
            ));
        }
        SessionFloorOutcome::Replay => AuthRejectionReason::Replay,
        SessionFloorOutcome::Stale => AuthRejectionReason::Stale,
    };
    record_auth_rejection(credential, reason);
    tracing::warn!(
        account_id = %account_id,
        signer_commitment = %signer_commitment,
        request_timestamp = %request_timestamp,
        outcome = ?outcome,
        "Replay rejected: timestamp not above the floor, or outside its window at the update"
    );
    Err(GuardianError::AuthenticationReplay)
}

pub fn normalize_payload(payload: Value) -> Result<Value> {
    let mut obj = payload.as_object().cloned().ok_or_else(|| {
        GuardianError::InvalidDelta("delta_payload must be an object".to_string())
    })?;

    let tx_summary = obj
        .get("tx_summary")
        .ok_or_else(|| GuardianError::InvalidDelta("Missing 'tx_summary' field".to_string()))?;
    validate_tx_summary(tx_summary)?;

    let metadata = obj
        .remove("metadata")
        .ok_or_else(|| GuardianError::InvalidDelta("Missing 'metadata' field".to_string()))?;
    let normalized_metadata = normalize_metadata(metadata)?;
    obj.insert("metadata".to_string(), normalized_metadata);

    Ok(Value::Object(obj))
}

fn validate_tx_summary(tx_summary: &Value) -> Result<()> {
    let obj = tx_summary.as_object().ok_or_else(|| {
        GuardianError::InvalidDelta("tx_summary must be an object with 'data' field".to_string())
    })?;

    let data = obj.get("data").and_then(Value::as_str).ok_or_else(|| {
        GuardianError::InvalidDelta("tx_summary.data must be a string".to_string())
    })?;

    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| {
            GuardianError::InvalidDelta(format!("tx_summary.data is not valid base64: {e}"))
        })?;
    Ok(())
}

fn normalize_metadata(metadata: Value) -> Result<Value> {
    let mut obj = metadata
        .as_object()
        .cloned()
        .ok_or_else(|| GuardianError::InvalidDelta("metadata must be a JSON object".to_string()))?;

    let proposal_type = obj
        .get("proposal_type")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            GuardianError::InvalidDelta("metadata.proposal_type is required".to_string())
        })?;
    let proposal_type = proposal_type.trim();
    if proposal_type.is_empty() {
        return Err(GuardianError::InvalidDelta(
            "metadata.proposal_type must not be empty".to_string(),
        ));
    }
    obj.insert(
        "proposal_type".to_string(),
        Value::String(proposal_type.to_string()),
    );

    obj.entry("description")
        .or_insert_with(|| Value::String(String::new()));

    if let Some(amount) = obj.get("amount") {
        if let Some(num) = amount.as_u64() {
            obj.insert("amount".to_string(), Value::String(num.to_string()));
        } else if let Some(num) = amount.as_i64() {
            obj.insert("amount".to_string(), Value::String(num.to_string()));
        }
    }

    if let Some(required_signatures) = obj.get("required_signatures") {
        let normalized = if let Some(num) = required_signatures.as_u64() {
            num
        } else if let Some(text) = required_signatures.as_str() {
            text.parse::<u64>().map_err(|_| {
                GuardianError::InvalidDelta(
                    "metadata.required_signatures must be a positive integer".to_string(),
                )
            })?
        } else {
            return Err(GuardianError::InvalidDelta(
                "metadata.required_signatures must be a positive integer".to_string(),
            ));
        };

        if normalized == 0 {
            return Err(GuardianError::InvalidDelta(
                "metadata.required_signatures must be greater than zero".to_string(),
            ));
        }

        obj.insert(
            "required_signatures".to_string(),
            Value::Number(serde_json::Number::from(normalized)),
        );
    }

    Ok(Value::Object(obj))
}

#[cfg(test)]
mod normalize_tests {
    use super::normalize_payload;
    use serde_json::{Value, json};

    #[test]
    fn normalize_payload_accepts_update_procedure_threshold_metadata() {
        let payload = json!({
            "tx_summary": { "data": "dGVzdA==" },
            "signatures": [],
            "metadata": {
                "proposal_type": "update_procedure_threshold",
                "target_threshold": 1,
                "target_procedure": "send_asset",
                "required_signatures": "2",
                "description": "set override"
            }
        });

        let normalized = normalize_payload(payload).expect("payload should normalize");
        let metadata = normalized
            .get("metadata")
            .and_then(Value::as_object)
            .expect("metadata should be an object");

        assert_eq!(
            metadata.get("proposal_type").and_then(Value::as_str),
            Some("update_procedure_threshold")
        );
        assert_eq!(
            metadata.get("target_procedure").and_then(Value::as_str),
            Some("send_asset")
        );
        assert_eq!(
            metadata.get("required_signatures").and_then(Value::as_u64),
            Some(2)
        );
    }

    #[test]
    fn normalize_payload_accepts_custom_proposal_type() {
        let payload = json!({
            "tx_summary": { "data": "dGVzdA==" },
            "signatures": [],
            "metadata": {
                "proposal_type": "b2agg",
                "description": "custom bridge note"
            }
        });

        let normalized = normalize_payload(payload).expect("custom type should normalize");
        let metadata = normalized
            .get("metadata")
            .and_then(Value::as_object)
            .expect("metadata should be an object");

        assert_eq!(
            metadata.get("proposal_type").and_then(Value::as_str),
            Some("b2agg")
        );
    }

    #[test]
    fn normalize_payload_persists_trimmed_proposal_type() {
        let payload = json!({
            "tx_summary": { "data": "dGVzdA==" },
            "signatures": [],
            "metadata": {
                "proposal_type": "  b2agg  ",
                "description": "padded label"
            }
        });

        let normalized = normalize_payload(payload).expect("padded type should normalize");
        let metadata = normalized
            .get("metadata")
            .and_then(Value::as_object)
            .expect("metadata should be an object");

        assert_eq!(
            metadata.get("proposal_type").and_then(Value::as_str),
            Some("b2agg"),
            "the stored proposal_type must be the trimmed value that was validated"
        );
    }

    #[test]
    fn normalize_payload_rejects_empty_proposal_type() {
        let payload = json!({
            "tx_summary": { "data": "dGVzdA==" },
            "signatures": [],
            "metadata": {
                "proposal_type": "   ",
                "description": "blank"
            }
        });

        let error = normalize_payload(payload).expect_err("blank proposal_type must be rejected");
        assert!(
            error
                .to_string()
                .contains("proposal_type must not be empty"),
            "unexpected error: {error}"
        );
    }

    /// Client fields the server does not model pass through unread, so a new
    /// SDK field needs no server deploy.
    #[test]
    fn normalize_payload_passes_bound_block_num_through() {
        let payload = json!({
            "tx_summary": { "data": "AAAA" },
            "metadata": { "proposal_type": "p2id", "bound_block_num": 42 },
        });

        let normalized = normalize_payload(payload).expect("payload normalizes");
        assert_eq!(normalized["metadata"]["bound_block_num"], json!(42));
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::ack::AckRegistry;
    use crate::builder::clock::Clock;
    use crate::builder::clock::test::MockClock;
    use crate::metadata::auth::Auth;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use chrono::{TimeZone, Utc};

    async fn create_test_state_with_mocks_and_clock(
        metadata: MockMetadataStore,
        clock: MockClock,
    ) -> AppState {
        let storage = MockStorageBackend::new();
        let network = MockNetworkClient::new();

        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("Failed to create keystore directory");
        let ack = AckRegistry::new(keystore_dir)
            .await
            .expect("Failed to create ack registry");

        AppState {
            execution: Default::default(),
            miden_sessions: std::sync::Arc::new(crate::session::MidenSessions::default()),
            storage: Arc::new(storage),
            metadata: Arc::new(metadata),
            network_client: Arc::new(network),
            ack,
            canonicalization: None,
            release_sweep: None,
            block_watch: None,
            clock: Arc::new(clock),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    fn create_account_metadata(account_id: String, commitments: Vec<String>) -> AccountMetadata {
        AccountMetadata {
            account_id,
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: commitments,
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    #[tokio::test]
    async fn test_resolve_account_timestamp_too_old() {
        // Set server clock to a specific time
        let clock = MockClock::new(Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap());
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (signer_pubkey, signer_commitment, _, _) =
            crate::testing::helpers::generate_falcon_signature(account_id);

        // Configure metadata mock
        let metadata = MockMetadataStore::new().with_get(Ok(Some(create_account_metadata(
            account_id.to_string(),
            vec![signer_commitment],
        ))));

        let state = create_test_state_with_mocks_and_clock(metadata, clock.clone()).await;

        // Create credentials with timestamp way in the past (10 minutes = 600000ms ago)
        let old_timestamp = clock.now().timestamp_millis() - 600_000;
        let (old_signature, _) = crate::testing::helpers::TestSigner::new()
            .sign_with_timestamp(account_id, old_timestamp);
        let creds = Credentials::signature(signer_pubkey, old_signature, old_timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::TimestampOutOfWindow { window_ms, .. } => {
                assert_eq!(window_ms, MAX_TIMESTAMP_SKEW_MS as u64);
            }
            e => panic!("Expected TimestampOutOfWindow, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_resolve_account_timestamp_in_future() {
        // Set server clock to a specific time
        let clock = MockClock::new(Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap());
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (signer_pubkey, signer_commitment, _, _) =
            crate::testing::helpers::generate_falcon_signature(account_id);

        // Configure metadata mock
        let metadata = MockMetadataStore::new().with_get(Ok(Some(create_account_metadata(
            account_id.to_string(),
            vec![signer_commitment],
        ))));

        let state = create_test_state_with_mocks_and_clock(metadata, clock.clone()).await;

        // Create credentials with timestamp way in the future (10 minutes = 600000ms ahead)
        let future_timestamp = clock.now().timestamp_millis() + 600_000;
        let (future_signature, _) = crate::testing::helpers::TestSigner::new()
            .sign_with_timestamp(account_id, future_timestamp);
        let creds = Credentials::signature(signer_pubkey, future_signature, future_timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::TimestampOutOfWindow { window_ms, .. } => {
                assert_eq!(window_ms, MAX_TIMESTAMP_SKEW_MS as u64);
            }
            e => panic!("Expected TimestampOutOfWindow, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_resolve_account_replay_attack_detected() {
        // Set server clock to a specific time
        let clock = MockClock::new(Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap());
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";

        // Create a signer and generate signature with the mock clock's timestamp
        let test_signer = crate::testing::helpers::TestSigner::new();
        let timestamp = clock.now().timestamp_millis();
        let (signature, _) = test_signer.sign_with_timestamp(account_id, timestamp);

        // Configure metadata mock with CAS returning false (replay detected)
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(create_account_metadata(
                account_id.to_string(),
                vec![test_signer.commitment_hex.clone()],
            ))))
            .with_update_timestamp_cas(Ok(false));

        let state = create_test_state_with_mocks_and_clock(metadata, clock).await;

        let creds = Credentials::signature(test_signer.pubkey_hex, signature, timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::AuthenticationReplay => {}
            e => panic!("Expected AuthenticationReplay, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_resolve_account_cas_storage_error() {
        // Set server clock to a specific time
        let clock = MockClock::new(Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap());
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";

        // Create a signer and generate signature with the mock clock's timestamp
        let test_signer = crate::testing::helpers::TestSigner::new();
        let timestamp = clock.now().timestamp_millis();
        let (signature, _) = test_signer.sign_with_timestamp(account_id, timestamp);

        // Configure metadata mock with CAS returning error
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(create_account_metadata(
                account_id.to_string(),
                vec![test_signer.commitment_hex.clone()],
            ))))
            .with_update_timestamp_cas(Err("Database connection failed".to_string()));

        let state = create_test_state_with_mocks_and_clock(metadata, clock).await;

        let creds = Credentials::signature(test_signer.pubkey_hex, signature, timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::StorageError(msg) => {
                assert!(msg.contains("Failed to update last auth timestamp"));
            }
            e => panic!("Expected StorageError, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_resolve_account_not_found() {
        let clock = MockClock::default();
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (signer_pubkey, _, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(account_id);

        // Configure metadata mock to return None (account not found)
        let metadata = MockMetadataStore::new().with_get(Ok(None));

        let state = create_test_state_with_mocks_and_clock(metadata, clock).await;

        let creds = Credentials::signature(signer_pubkey, signer_signature, signer_timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::AccountNotFound(_) => {}
            e => panic!("Expected AccountNotFound, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_resolve_account_metadata_storage_error() {
        let clock = MockClock::default();
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (signer_pubkey, _, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(account_id);

        // Configure metadata mock to return error
        let metadata = MockMetadataStore::new().with_get(Err("Database error".to_string()));

        let state = create_test_state_with_mocks_and_clock(metadata, clock).await;

        let creds = Credentials::signature(signer_pubkey, signer_signature, signer_timestamp);

        let result = resolve_account(&state, account_id, &creds).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::StorageError(msg) => {
                assert!(msg.contains("Failed to check account"));
            }
            e => panic!("Expected StorageError, got: {:?}", e),
        }
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod replay_protection_tests {
    use super::*;
    use crate::error::GuardianError;
    use crate::metadata::{AccountMetadata, Auth};
    use crate::state::AppState;
    use crate::testing::helpers::{TestSigner, create_test_app_state};

    const ACCOUNT_ID: &str = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";

    async fn app_state_with_account(auth: Auth) -> AppState {
        let state = create_test_app_state().await;
        state
            .metadata
            .set(AccountMetadata {
                account_id: ACCOUNT_ID.to_string(),
                auth,
                network_config: crate::metadata::NetworkConfig::miden_default(),
                created_at: "2024-01-01T00:00:00Z".to_string(),
                updated_at: "2024-01-01T00:00:00Z".to_string(),
                has_pending_candidate: false,
                paused_at: None,
                paused_reason: None,
                released_at: None,
            })
            .await
            .expect("account metadata stored");
        state
    }

    fn assert_replay_rejected(result: Result<ResolvedAccount>) {
        match result.expect_err("replayed request must be rejected") {
            GuardianError::AuthenticationReplay => {}
            e => panic!("Expected AuthenticationReplay, got: {e:?}"),
        }
    }

    #[tokio::test]
    async fn first_request_accepted_then_identical_replay_rejected() {
        let signer = TestSigner::new();
        let state = app_state_with_account(Auth::MidenFalconRpo {
            cosigner_commitments: vec![signer.commitment_hex.clone()],
        })
        .await;
        let timestamp = chrono::Utc::now().timestamp_millis();
        let (signature, _) = signer.sign_with_timestamp(ACCOUNT_ID, timestamp);
        let creds = Credentials::signature(signer.pubkey_hex.clone(), signature, timestamp);

        resolve_account(&state, ACCOUNT_ID, &creds)
            .await
            .expect("first request accepted");
        assert_replay_rejected(resolve_account(&state, ACCOUNT_ID, &creds).await);
    }

    fn auth_rejection_lines(run: impl std::future::Future<Output = ()>) -> Vec<String> {
        let recorder = crate::metrics::recorder::build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run)
        });
        handle
            .render()
            .lines()
            .filter(|line| line.starts_with("guardian_auth_rejections_total{"))
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn wallet_rejections_are_counted_by_reason() {
        let lines = auth_rejection_lines(async {
            let signer = TestSigner::new();
            let state = app_state_with_account(Auth::MidenFalconRpo {
                cosigner_commitments: vec![signer.commitment_hex.clone()],
            })
            .await;
            let now = chrono::Utc::now().timestamp_millis();

            let (signature, _) = signer.sign_with_timestamp(ACCOUNT_ID, now);
            let creds = Credentials::signature(signer.pubkey_hex.clone(), signature, now);
            resolve_account(&state, ACCOUNT_ID, &creds).await.unwrap();
            let _ = resolve_account(&state, ACCOUNT_ID, &creds).await;

            let stale = now - MAX_TIMESTAMP_SKEW_MS - 1_000;
            let (signature, _) = signer.sign_with_timestamp(ACCOUNT_ID, stale);
            let creds = Credentials::signature(signer.pubkey_hex.clone(), signature, stale);
            let _ = resolve_account(&state, ACCOUNT_ID, &creds).await;

            let (signature, _) = TestSigner::new().sign_with_timestamp(ACCOUNT_ID, now + 1);
            let creds = Credentials::signature(signer.pubkey_hex.clone(), signature, now + 1);
            let _ = resolve_account(&state, ACCOUNT_ID, &creds).await;
        });

        for reason in ["replay", "timestamp_window", "invalid_signature"] {
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("credential=\"wallet\"")
                        && line.contains(&format!("reason=\"{reason}\""))
                        && line.ends_with(" 1")),
                "missing {reason} in {lines:?}"
            );
        }
    }

    #[tokio::test]
    async fn timestamp_older_than_last_accepted_is_rejected() {
        let signer = TestSigner::new();
        let state = app_state_with_account(Auth::MidenFalconRpo {
            cosigner_commitments: vec![signer.commitment_hex.clone()],
        })
        .await;
        let newer = chrono::Utc::now().timestamp_millis();
        let older = newer - 1_000;

        let (newer_signature, _) = signer.sign_with_timestamp(ACCOUNT_ID, newer);
        resolve_account(
            &state,
            ACCOUNT_ID,
            &Credentials::signature(signer.pubkey_hex.clone(), newer_signature, newer),
        )
        .await
        .expect("request at the newer timestamp accepted");

        let (older_signature, _) = signer.sign_with_timestamp(ACCOUNT_ID, older);
        assert_replay_rejected(
            resolve_account(
                &state,
                ACCOUNT_ID,
                &Credentials::signature(signer.pubkey_hex.clone(), older_signature, older),
            )
            .await,
        );
    }

    #[tokio::test]
    async fn cosigners_do_not_contend_on_replay_state() {
        let signer_a = TestSigner::new();
        let signer_b = TestSigner::new();
        let state = app_state_with_account(Auth::MidenFalconRpo {
            cosigner_commitments: vec![
                signer_a.commitment_hex.clone(),
                signer_b.commitment_hex.clone(),
            ],
        })
        .await;

        let newer = chrono::Utc::now().timestamp_millis();
        let older = newer - 60_000;

        let (signature_a, _) = signer_a.sign_with_timestamp(ACCOUNT_ID, newer);
        resolve_account(
            &state,
            ACCOUNT_ID,
            &Credentials::signature(signer_a.pubkey_hex.clone(), signature_a, newer),
        )
        .await
        .expect("first cosigner accepted");

        let (signature_b, _) = signer_b.sign_with_timestamp(ACCOUNT_ID, older);
        resolve_account(
            &state,
            ACCOUNT_ID,
            &Credentials::signature(signer_b.pubkey_hex.clone(), signature_b, older),
        )
        .await
        .expect("second cosigner accepted although its clock trails the first cosigner's");

        let (replayed_b, _) = signer_b.sign_with_timestamp(ACCOUNT_ID, older);
        assert_replay_rejected(
            resolve_account(
                &state,
                ACCOUNT_ID,
                &Credentials::signature(signer_b.pubkey_hex.clone(), replayed_b, older),
            )
            .await,
        );
    }

    #[tokio::test]
    async fn same_signer_clock_ahead_within_skew_is_a_retryable_replay_not_terminal() {
        let signer = TestSigner::new();
        let state = app_state_with_account(Auth::MidenFalconRpo {
            cosigner_commitments: vec![signer.commitment_hex.clone()],
        })
        .await;

        let now = chrono::Utc::now().timestamp_millis();
        let ahead = now + 60_000;

        let (ahead_signature, _) = signer.sign_with_timestamp(ACCOUNT_ID, ahead);
        resolve_account(
            &state,
            ACCOUNT_ID,
            &Credentials::signature(signer.pubkey_hex.clone(), ahead_signature, ahead),
        )
        .await
        .expect("a fast clock within the skew window is accepted");

        let (wall_clock_signature, _) = signer.sign_with_timestamp(ACCOUNT_ID, now);
        assert_replay_rejected(
            resolve_account(
                &state,
                ACCOUNT_ID,
                &Credentials::signature(signer.pubkey_hex.clone(), wall_clock_signature, now),
            )
            .await,
        );
    }

    #[tokio::test]
    async fn in_flight_reorder_of_adjacent_timestamps_rejects_only_the_late_arrival() {
        let signer = TestSigner::new();
        let state = app_state_with_account(Auth::MidenFalconRpo {
            cosigner_commitments: vec![signer.commitment_hex.clone()],
        })
        .await;

        let first = chrono::Utc::now().timestamp_millis();
        let second = first + 1;
        let (first_signature, _) = signer.sign_with_timestamp(ACCOUNT_ID, first);
        let (second_signature, _) = signer.sign_with_timestamp(ACCOUNT_ID, second);

        resolve_account(
            &state,
            ACCOUNT_ID,
            &Credentials::signature(signer.pubkey_hex.clone(), second_signature, second),
        )
        .await
        .expect("the later-minted request lands first and is accepted");

        assert_replay_rejected(
            resolve_account(
                &state,
                ACCOUNT_ID,
                &Credentials::signature(signer.pubkey_hex.clone(), first_signature, first),
            )
            .await,
        );

        let third = second + 1;
        let (third_signature, _) = signer.sign_with_timestamp(ACCOUNT_ID, third);
        resolve_account(
            &state,
            ACCOUNT_ID,
            &Credentials::signature(signer.pubkey_hex.clone(), third_signature, third),
        )
        .await
        .expect("a fresh timestamp minted after the rejection is accepted");
    }

    #[tokio::test]
    async fn ecdsa_replay_scope_is_per_signer() {
        use crate::testing::helpers::TestEcdsaSigner;

        let signer_a = TestEcdsaSigner::new();
        let signer_b = TestEcdsaSigner::new();
        let state = app_state_with_account(Auth::MidenEcdsa {
            cosigner_commitments: vec![
                signer_a.commitment_hex.clone(),
                signer_b.commitment_hex.clone(),
            ],
        })
        .await;

        let newer = chrono::Utc::now().timestamp_millis();
        let older = newer - 60_000;

        let (signature_a, _) = signer_a.sign_with_timestamp(ACCOUNT_ID, newer);
        resolve_account(
            &state,
            ACCOUNT_ID,
            &Credentials::signature(signer_a.pubkey_hex.clone(), signature_a, newer),
        )
        .await
        .expect("first ECDSA cosigner accepted");

        let (signature_b, _) = signer_b.sign_with_timestamp(ACCOUNT_ID, older);
        resolve_account(
            &state,
            ACCOUNT_ID,
            &Credentials::signature(signer_b.pubkey_hex.clone(), signature_b, older),
        )
        .await
        .expect("second ECDSA cosigner accepted although its clock trails the first cosigner's");

        let (replayed_b, _) = signer_b.sign_with_timestamp(ACCOUNT_ID, older);
        assert_replay_rejected(
            resolve_account(
                &state,
                ACCOUNT_ID,
                &Credentials::signature(signer_b.pubkey_hex.clone(), replayed_b, older),
            )
            .await,
        );

        let fresher = older + 1;
        let (fresh_b, _) = signer_b.sign_with_timestamp(ACCOUNT_ID, fresher);
        resolve_account(
            &state,
            ACCOUNT_ID,
            &Credentials::signature(signer_b.pubkey_hex.clone(), fresh_b, fresher),
        )
        .await
        .expect("the ECDSA signer recovers with a fresh timestamp");
    }

    #[tokio::test]
    async fn replay_state_survives_metadata_reconfiguration() {
        let signer = TestSigner::new();
        let state = app_state_with_account(Auth::MidenFalconRpo {
            cosigner_commitments: vec![signer.commitment_hex.clone()],
        })
        .await;
        let timestamp = chrono::Utc::now().timestamp_millis();
        let (signature, _) = signer.sign_with_timestamp(ACCOUNT_ID, timestamp);
        let creds = Credentials::signature(signer.pubkey_hex.clone(), signature, timestamp);

        resolve_account(&state, ACCOUNT_ID, &creds)
            .await
            .expect("first request accepted");

        let stale_read = state
            .metadata
            .get(ACCOUNT_ID)
            .await
            .expect("metadata read")
            .expect("account exists");
        state
            .metadata
            .set(stale_read)
            .await
            .expect("metadata rewritten from a stale read");

        assert_replay_rejected(resolve_account(&state, ACCOUNT_ID, &creds).await);
    }

    fn assert_endpoint_replay_rejected(
        endpoint: &str,
        first: std::result::Result<(), GuardianError>,
        second: std::result::Result<(), GuardianError>,
    ) {
        if let Err(GuardianError::AuthenticationFailed(msg)) = &first {
            panic!("{endpoint}: first request failed authentication: {msg}");
        }
        match second.expect_err("replayed request must be rejected") {
            GuardianError::AuthenticationReplay => {}
            e => panic!("{endpoint}: expected AuthenticationReplay, got: {e:?}"),
        }
    }

    #[tokio::test]
    async fn every_authenticated_read_endpoint_rejects_a_replayed_request() {
        for endpoint in [
            "get_state",
            "get_state_tail",
            "get_delta_since",
            "get_delta_proposals",
            "get_delta_proposal",
            "get_delta",
        ] {
            let signer = TestSigner::new();
            let state = app_state_with_account(Auth::MidenFalconRpo {
                cosigner_commitments: vec![signer.commitment_hex.clone()],
            })
            .await;
            let timestamp = chrono::Utc::now().timestamp_millis();
            let (signature, _) = signer.sign_with_timestamp(ACCOUNT_ID, timestamp);
            let credentials =
                Credentials::signature(signer.pubkey_hex.clone(), signature, timestamp);

            let call = async |credentials: Credentials| match endpoint {
                "get_state" => get_state(
                    &state,
                    GetStateParams {
                        account_id: ACCOUNT_ID.to_string(),
                        credentials,
                    },
                )
                .await
                .map(|_| ()),
                "get_state_tail" => get_state_tail(
                    &state,
                    GetStateTailParams {
                        account_id: ACCOUNT_ID.to_string(),
                        credentials,
                    },
                )
                .await
                .map(|_| ()),
                "get_delta_since" => get_delta_since(
                    &state,
                    GetDeltaSinceParams {
                        account_id: ACCOUNT_ID.to_string(),
                        from_nonce: 0,
                        credentials,
                    },
                )
                .await
                .map(|_| ()),
                "get_delta_proposals" => get_delta_proposals(
                    &state,
                    GetDeltaProposalsParams {
                        account_id: ACCOUNT_ID.to_string(),
                        credentials,
                    },
                )
                .await
                .map(|_| ()),
                "get_delta_proposal" => get_delta_proposal(
                    &state,
                    GetDeltaProposalParams {
                        account_id: ACCOUNT_ID.to_string(),
                        commitment: "0xabc".to_string(),
                        credentials,
                    },
                )
                .await
                .map(|_| ()),
                "get_delta" => get_delta(
                    &state,
                    GetDeltaParams {
                        account_id: ACCOUNT_ID.to_string(),
                        nonce: 1,
                        credentials,
                    },
                )
                .await
                .map(|_| ()),
                other => panic!("unknown endpoint {other}"),
            };

            let first = call(credentials.clone()).await;
            let second = call(credentials).await;
            assert_endpoint_replay_rejected(endpoint, first, second);
        }
    }
}
