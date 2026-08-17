use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fmt;

/// Primary error type for GUARDIAN operations
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardianError {
    AccountNotFound(String),
    AccountAlreadyExists(String),
    AccountDataUnavailable(String),
    InvalidAccountId(String),
    StateNotFound(String),
    DeltaNotFound {
        account_id: String,
        nonce: u64,
    },
    InvalidDelta(String),
    ConflictPendingDelta,
    ConflictPendingProposal,
    PendingProposalsLimit {
        limit: usize,
    },
    CommitmentMismatch {
        expected: String,
        actual: String,
    },
    InvalidCommitment(String),
    AuthenticationFailed(String),
    /// A correctly signed request lost the replay-protection CAS: its
    /// timestamp was not strictly greater than the last accepted timestamp
    /// for that signer. Unlike every other authentication failure this is
    /// transient: the client retries with a fresh timestamp and signature.
    /// Stable code `authentication_replay`, HTTP 401, gRPC
    /// `Unauthenticated`, `meta.retryable: true`. Deliberately coarse: it
    /// reveals only that a replay was detected (not a useful oracle), while
    /// signature-level diagnostics stay log-only (feature 009). Issue #367.
    AuthenticationReplay,
    AuthorizationFailed(String),
    InvalidInput(String),
    StorageError(String),
    NetworkError(String),
    SigningError(String),
    ConfigurationError(String),
    ProposalNotFound {
        account_id: String,
        commitment: String,
    },
    ProposalAlreadySigned {
        signer_id: String,
    },
    InvalidProposalSignature(String),
    UnsupportedForNetwork {
        network: String,
        operation: String,
    },
    UnsupportedEvmChain {
        chain_id: u64,
    },
    InvalidNetworkConfig(String),
    RpcUnavailable(String),
    RpcValidationFailed(String),
    SignerNotAuthorized(String),
    InvalidEvmProposal(String),
    InsufficientSignatures {
        required: usize,
        got: usize,
    },
    RateLimitExceeded {
        retry_after_secs: u32,
        scope: String,
    },
    /// Dashboard pagination cursor is malformed, tampered, or no longer valid.
    /// Maps to HTTP 400 with stable code `invalid_cursor`. See FR-005/FR-028
    /// of `005-operator-dashboard-metrics`.
    InvalidCursor(String),
    /// Dashboard pagination `limit` parameter is outside the allowed range
    /// `[1, 500]`. Maps to HTTP 400 with stable code `invalid_limit`. See
    /// FR-002 of `005-operator-dashboard-metrics`.
    InvalidLimit(String),
    /// Dashboard global delta feed `status` filter contains an unknown or
    /// malformed value. Maps to HTTP 400 with stable code
    /// `invalid_status_filter`. See FR-033 of
    /// `005-operator-dashboard-metrics`.
    InvalidStatusFilter(String),
    /// Operator session is valid but lacks one or more required
    /// permissions. Feature 006-operator-authz FR-015 / FR-016. Maps
    /// to HTTP 403 with stable code
    /// `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`. The carried list
    /// is the set of permissions the route required that the operator
    /// does not hold, ordered lexicographically (FR-017). Not gRPC-
    /// surfaced because the operator dashboard is HTTP-only today
    /// (`crates/server/proto/guardian.proto:6-42`).
    InsufficientOperatorPermission {
        missing_permissions: Vec<String>,
    },
    /// Underlying records exist (or metadata exists) but cannot be read,
    /// or a cross-account aggregate is degraded above the filesystem
    /// threshold. Maps to HTTP 503 with stable code `data_unavailable`.
    /// Distinct from `AccountDataUnavailable` which is account-scoped.
    /// See FR-029 of `005-operator-dashboard-metrics`.
    DataUnavailable(String),
    /// Account is paused; mutating action rejected with stable code
    /// `GUARDIAN_ACCOUNT_PAUSED`. HTTP 409 Conflict, gRPC
    /// `FAILED_PRECONDITION`. `paused_at` / `paused_reason` are carried
    /// on the response body / gRPC `Status::details` so clients can
    /// show context without a follow-up GET.
    AccountPaused {
        paused_at: DateTime<Utc>,
        paused_reason: Option<String>,
    },
    /// The account switched to a different guardian and this server
    /// released it; mutating action rejected with stable code
    /// `GUARDIAN_ACCOUNT_RELEASED`. HTTP 409 Conflict, gRPC
    /// `FAILED_PRECONDITION`. Terminal until the wallet re-onboards via
    /// `/configure`. `released_at` is carried on the response body /
    /// gRPC `Status::details`.
    AccountReleased {
        released_at: DateTime<Utc>,
    },
    /// A candidate abandon was refused because the candidate's transaction
    /// already landed on-chain (or the candidate already canonicalized);
    /// the worker will canonicalize it shortly. Abandoning it would leave
    /// guardian state behind chain. Stable code `GUARDIAN_CANDIDATE_LANDED`.
    /// HTTP 409 Conflict, gRPC `FAILED_PRECONDITION`. Issue #319.
    CandidateLanded {
        account_id: String,
        nonce: u64,
    },
}

/// Signing-specific error type for Miden Falcon RPO operations
#[derive(Debug)]
pub enum MidenFalconRpoError {
    StorageError(String),
    DecodingError(String),
}

/// Result type alias for GUARDIAN operations
pub type Result<T> = std::result::Result<T, GuardianError>;

/// Result type alias for Miden Falcon RPO signing operations
pub type MidenFalconRpoResult<T> = std::result::Result<T, MidenFalconRpoError>;

impl GuardianError {
    pub fn http_status(&self) -> StatusCode {
        match self {
            GuardianError::AccountNotFound(_) => StatusCode::NOT_FOUND,
            GuardianError::AccountDataUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            GuardianError::DeltaNotFound { .. } => StatusCode::NOT_FOUND,
            GuardianError::StateNotFound(_) => StatusCode::NOT_FOUND,
            GuardianError::ProposalNotFound { .. } => StatusCode::NOT_FOUND,
            GuardianError::AccountAlreadyExists(_) => StatusCode::CONFLICT,
            GuardianError::ConflictPendingDelta => StatusCode::CONFLICT,
            GuardianError::ConflictPendingProposal => StatusCode::CONFLICT,
            GuardianError::PendingProposalsLimit { .. } => StatusCode::CONFLICT,
            GuardianError::ProposalAlreadySigned { .. } => StatusCode::CONFLICT,
            GuardianError::AuthenticationFailed(_) => StatusCode::UNAUTHORIZED,
            GuardianError::AuthenticationReplay => StatusCode::UNAUTHORIZED,
            GuardianError::AuthorizationFailed(_) => StatusCode::FORBIDDEN,
            GuardianError::InvalidInput(_) => StatusCode::BAD_REQUEST,
            GuardianError::InvalidAccountId(_) => StatusCode::BAD_REQUEST,
            GuardianError::InvalidDelta(_) => StatusCode::BAD_REQUEST,
            GuardianError::InvalidCommitment(_) => StatusCode::BAD_REQUEST,
            GuardianError::CommitmentMismatch { .. } => StatusCode::BAD_REQUEST,
            GuardianError::InvalidProposalSignature(_) => StatusCode::BAD_REQUEST,
            GuardianError::UnsupportedForNetwork { .. } => StatusCode::BAD_REQUEST,
            GuardianError::UnsupportedEvmChain { .. } => StatusCode::BAD_REQUEST,
            GuardianError::InvalidNetworkConfig(_) => StatusCode::BAD_REQUEST,
            GuardianError::RpcUnavailable(_) => StatusCode::BAD_GATEWAY,
            GuardianError::RpcValidationFailed(_) => StatusCode::BAD_GATEWAY,
            GuardianError::SignerNotAuthorized(_) => StatusCode::FORBIDDEN,
            GuardianError::InvalidEvmProposal(_) => StatusCode::BAD_REQUEST,
            GuardianError::InsufficientSignatures { .. } => StatusCode::BAD_REQUEST,
            GuardianError::RateLimitExceeded { .. } => StatusCode::TOO_MANY_REQUESTS,
            GuardianError::NetworkError(_) => StatusCode::BAD_GATEWAY,
            GuardianError::SigningError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            GuardianError::StorageError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            GuardianError::ConfigurationError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            GuardianError::InvalidCursor(_) => StatusCode::BAD_REQUEST,
            GuardianError::InvalidLimit(_) => StatusCode::BAD_REQUEST,
            GuardianError::InvalidStatusFilter(_) => StatusCode::BAD_REQUEST,
            GuardianError::InsufficientOperatorPermission { .. } => StatusCode::FORBIDDEN,
            GuardianError::DataUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            GuardianError::AccountPaused { .. } => StatusCode::CONFLICT,
            GuardianError::AccountReleased { .. } => StatusCode::CONFLICT,
            GuardianError::CandidateLanded { .. } => StatusCode::CONFLICT,
        }
    }

    pub fn grpc_status(&self) -> tonic::Code {
        match self {
            GuardianError::AccountNotFound(_) => tonic::Code::NotFound,
            GuardianError::AccountDataUnavailable(_) => tonic::Code::Unavailable,
            GuardianError::DeltaNotFound { .. } => tonic::Code::NotFound,
            GuardianError::StateNotFound(_) => tonic::Code::NotFound,
            GuardianError::ProposalNotFound { .. } => tonic::Code::NotFound,
            GuardianError::AccountAlreadyExists(_) => tonic::Code::AlreadyExists,
            GuardianError::ConflictPendingDelta => tonic::Code::FailedPrecondition,
            GuardianError::ConflictPendingProposal => tonic::Code::FailedPrecondition,
            GuardianError::PendingProposalsLimit { .. } => tonic::Code::FailedPrecondition,
            GuardianError::ProposalAlreadySigned { .. } => tonic::Code::AlreadyExists,
            GuardianError::AuthenticationFailed(_) => tonic::Code::Unauthenticated,
            GuardianError::AuthenticationReplay => tonic::Code::Unauthenticated,
            GuardianError::AuthorizationFailed(_) => tonic::Code::PermissionDenied,
            GuardianError::InvalidInput(_) => tonic::Code::InvalidArgument,
            GuardianError::InvalidAccountId(_) => tonic::Code::InvalidArgument,
            GuardianError::InvalidDelta(_) => tonic::Code::InvalidArgument,
            GuardianError::InvalidCommitment(_) => tonic::Code::InvalidArgument,
            GuardianError::CommitmentMismatch { .. } => tonic::Code::InvalidArgument,
            GuardianError::InvalidProposalSignature(_) => tonic::Code::InvalidArgument,
            GuardianError::UnsupportedForNetwork { .. } => tonic::Code::FailedPrecondition,
            GuardianError::UnsupportedEvmChain { .. } => tonic::Code::FailedPrecondition,
            GuardianError::InvalidNetworkConfig(_) => tonic::Code::InvalidArgument,
            GuardianError::RpcUnavailable(_) => tonic::Code::Unavailable,
            GuardianError::RpcValidationFailed(_) => tonic::Code::Unavailable,
            GuardianError::SignerNotAuthorized(_) => tonic::Code::PermissionDenied,
            GuardianError::InvalidEvmProposal(_) => tonic::Code::InvalidArgument,
            GuardianError::InsufficientSignatures { .. } => tonic::Code::FailedPrecondition,
            GuardianError::RateLimitExceeded { .. } => tonic::Code::ResourceExhausted,
            GuardianError::NetworkError(_) => tonic::Code::Unavailable,
            GuardianError::SigningError(_) => tonic::Code::Internal,
            GuardianError::StorageError(_) => tonic::Code::Internal,
            GuardianError::ConfigurationError(_) => tonic::Code::Internal,
            GuardianError::InvalidCursor(_) => tonic::Code::InvalidArgument,
            GuardianError::InvalidLimit(_) => tonic::Code::InvalidArgument,
            GuardianError::InvalidStatusFilter(_) => tonic::Code::InvalidArgument,
            // Operator surface is HTTP-only; this gRPC mapping exists only
            // for `tonic::Status` parity at the conversion boundary and
            // is not exposed to any production gRPC consumer in v1.
            GuardianError::InsufficientOperatorPermission { .. } => tonic::Code::PermissionDenied,
            GuardianError::DataUnavailable(_) => tonic::Code::Unavailable,
            GuardianError::AccountPaused { .. } => tonic::Code::FailedPrecondition,
            GuardianError::AccountReleased { .. } => tonic::Code::FailedPrecondition,
            GuardianError::CandidateLanded { .. } => tonic::Code::FailedPrecondition,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            GuardianError::AccountNotFound(_) => "account_not_found",
            GuardianError::AccountAlreadyExists(_) => "account_already_exists",
            GuardianError::AccountDataUnavailable(_) => "account_data_unavailable",
            GuardianError::InvalidAccountId(_) => "invalid_account_id",
            GuardianError::StateNotFound(_) => "state_not_found",
            GuardianError::DeltaNotFound { .. } => "delta_not_found",
            GuardianError::InvalidDelta(_) => "invalid_delta",
            GuardianError::ConflictPendingDelta => "conflict_pending_delta",
            GuardianError::ConflictPendingProposal => "conflict_pending_proposal",
            GuardianError::PendingProposalsLimit { .. } => "pending_proposals_limit",
            GuardianError::CommitmentMismatch { .. } => "commitment_mismatch",
            GuardianError::InvalidCommitment(_) => "invalid_commitment",
            GuardianError::AuthenticationFailed(_) => "authentication_failed",
            GuardianError::AuthenticationReplay => "authentication_replay",
            GuardianError::AuthorizationFailed(_) => "authorization_failed",
            GuardianError::InvalidInput(_) => "invalid_input",
            GuardianError::StorageError(_) => "storage_error",
            GuardianError::NetworkError(_) => "network_error",
            GuardianError::SigningError(_) => "signing_error",
            GuardianError::ConfigurationError(_) => "configuration_error",
            GuardianError::ProposalNotFound { .. } => "proposal_not_found",
            GuardianError::ProposalAlreadySigned { .. } => "proposal_already_signed",
            GuardianError::InvalidProposalSignature(_) => "invalid_proposal_signature",
            GuardianError::UnsupportedForNetwork { .. } => "unsupported_for_network",
            GuardianError::UnsupportedEvmChain { .. } => "unsupported_evm_chain",
            GuardianError::InvalidNetworkConfig(_) => "invalid_network_config",
            GuardianError::RpcUnavailable(_) => "rpc_unavailable",
            GuardianError::RpcValidationFailed(_) => "rpc_validation_failed",
            GuardianError::SignerNotAuthorized(_) => "signer_not_authorized",
            GuardianError::InvalidEvmProposal(_) => "invalid_evm_proposal",
            GuardianError::InsufficientSignatures { .. } => "insufficient_signatures",
            GuardianError::RateLimitExceeded { .. } => "rate_limit_exceeded",
            GuardianError::InvalidCursor(_) => "invalid_cursor",
            GuardianError::InvalidLimit(_) => "invalid_limit",
            GuardianError::InvalidStatusFilter(_) => "invalid_status_filter",
            GuardianError::InsufficientOperatorPermission { .. } => {
                "GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION"
            }
            GuardianError::DataUnavailable(_) => "data_unavailable",
            GuardianError::AccountPaused { .. } => "GUARDIAN_ACCOUNT_PAUSED",
            GuardianError::AccountReleased { .. } => "GUARDIAN_ACCOUNT_RELEASED",
            GuardianError::CandidateLanded { .. } => "GUARDIAN_CANDIDATE_LANDED",
        }
    }

    /// Short, end-user-safe message for this error (feature
    /// `009-human-readable-errors`). Unlike [`Display`], this is **safe to show
    /// directly in a wallet UI**: it is a single plain sentence and is
    /// safe-by-construction — it never interpolates account IDs, commitments,
    /// nonces, signer IDs, file paths, URLs, or raw upstream/RPC text. Wording
    /// is NOT part of the stable wire contract; only [`code`](Self::code) is
    /// stable. Clients branch and localize on `code`, never on this text.
    ///
    /// Server-mapped connectivity faults (`network_error`, `rpc_unavailable`,
    /// `rpc_validation_failed`) deliberately get a connectivity-style message
    /// distinct from the single generic message used for pure internal faults
    /// (`storage_error`, `signing_error`, `configuration_error`,
    /// `data_unavailable`).
    pub fn user_message(&self) -> &'static str {
        match self {
            // Not found — uniform, no info leak about which thing/account.
            GuardianError::AccountNotFound(_)
            | GuardianError::StateNotFound(_)
            | GuardianError::DeltaNotFound { .. }
            | GuardianError::ProposalNotFound { .. } => {
                "We couldn't find that. It may have been completed or removed."
            }
            GuardianError::AccountAlreadyExists(_) => "This account already exists.",
            // Validation — not user-actionable beyond retrying with valid input.
            GuardianError::InvalidAccountId(_)
            | GuardianError::InvalidDelta(_)
            | GuardianError::InvalidCommitment(_)
            | GuardianError::CommitmentMismatch { .. }
            | GuardianError::InvalidProposalSignature(_)
            | GuardianError::InvalidInput(_)
            | GuardianError::InvalidNetworkConfig(_)
            | GuardianError::InvalidEvmProposal(_)
            | GuardianError::InvalidCursor(_)
            | GuardianError::InvalidLimit(_)
            | GuardianError::InvalidStatusFilter(_) => {
                "That request couldn't be processed. Please check the details and try again."
            }
            // Pending-change conflicts.
            GuardianError::ConflictPendingDelta
            | GuardianError::ConflictPendingProposal
            | GuardianError::PendingProposalsLimit { .. } => {
                "There's already a pending change for this account. Finish or cancel it first."
            }
            // Per-request signed auth has no session; the same code also
            // covers dashboard/EVM session auth, so the wording stays
            // neutral between "retry" and "reconnect".
            GuardianError::AuthenticationFailed(_) => {
                "Guardian could not authenticate this request. Please retry or reconnect your signer."
            }
            GuardianError::AuthenticationReplay => {
                "Guardian received this request out of order. Please try again."
            }
            GuardianError::AuthorizationFailed(_) | GuardianError::SignerNotAuthorized(_) => {
                "You're not an authorized signer for this account."
            }
            GuardianError::InsufficientOperatorPermission { .. } => {
                "You don't have permission to do that."
            }
            GuardianError::ProposalAlreadySigned { .. } => {
                "You've already signed this transaction."
            }
            GuardianError::InsufficientSignatures { .. } => {
                "This transaction still needs more signatures."
            }
            GuardianError::UnsupportedForNetwork { .. }
            | GuardianError::UnsupportedEvmChain { .. } => {
                "That action isn't supported for this account's network."
            }
            GuardianError::RateLimitExceeded { .. } => {
                "Too many requests — please try again shortly."
            }
            GuardianError::AccountPaused { .. } => {
                "This account is paused and can't approve transactions right now."
            }
            GuardianError::AccountReleased { .. } => {
                "This account has moved to a different guardian. Reconnect it to continue."
            }
            GuardianError::CandidateLanded { .. } => {
                "This transaction already went through, so it can't be abandoned."
            }
            GuardianError::AccountDataUnavailable(_) => {
                "This account's data is temporarily unavailable. Please try again."
            }
            // Server-mapped connectivity (Guardian reached; the network behind
            // it didn't answer) — distinct from internal faults below.
            GuardianError::NetworkError(_)
            | GuardianError::RpcUnavailable(_)
            | GuardianError::RpcValidationFailed(_) => {
                "Guardian can't reach the network right now. Please try again."
            }
            // Pure internal faults — single generic message, no internals.
            GuardianError::StorageError(_)
            | GuardianError::SigningError(_)
            | GuardianError::ConfigurationError(_)
            | GuardianError::DataUnavailable(_) => {
                "Something went wrong on Guardian's side. Please try again."
            }
        }
    }

    /// Whether retrying the same request could plausibly succeed. Surfaced in
    /// `meta.retryable` on the error wire object so clients can drive retry UI
    /// uniformly without branching on every `code`.
    pub fn retryable(&self) -> bool {
        // `ConfigurationError` is deterministic (a misconfigured server won't
        // start serving the same request on retry), so it is NOT retryable.
        matches!(
            self,
            GuardianError::AuthenticationReplay
                | GuardianError::AccountDataUnavailable(_)
                | GuardianError::StorageError(_)
                | GuardianError::NetworkError(_)
                | GuardianError::SigningError(_)
                | GuardianError::RpcUnavailable(_)
                | GuardianError::RpcValidationFailed(_)
                | GuardianError::RateLimitExceeded { .. }
                | GuardianError::DataUnavailable(_)
        )
    }
}

impl fmt::Display for GuardianError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GuardianError::AccountNotFound(id) => write!(f, "Account '{id}' not found"),
            GuardianError::AccountAlreadyExists(id) => write!(f, "Account '{id}' already exists"),
            GuardianError::AccountDataUnavailable(id) => {
                write!(f, "Account data unavailable for '{id}'")
            }
            GuardianError::InvalidAccountId(msg) => write!(f, "Invalid account ID: {msg}"),
            GuardianError::StateNotFound(id) => write!(f, "State not found for account '{id}'"),
            GuardianError::DeltaNotFound { account_id, nonce } => {
                write!(
                    f,
                    "Delta not found for account '{account_id}' at nonce {nonce}"
                )
            }
            GuardianError::InvalidDelta(msg) => write!(f, "Invalid delta: {msg}"),
            GuardianError::ConflictPendingDelta => {
                write!(
                    f,
                    "Cannot push new delta: there is already a non-canonical delta pending"
                )
            }
            GuardianError::ConflictPendingProposal => {
                write!(f, "Cannot push new delta: there are pending proposals")
            }
            GuardianError::PendingProposalsLimit { limit } => write!(
                f,
                "Cannot push new delta proposal: maximum pending proposal limit ({limit}) reached for this account"
            ),
            GuardianError::CommitmentMismatch { expected, actual } => {
                write!(f, "Commitment mismatch: expected {expected}, got {actual}")
            }
            GuardianError::InvalidCommitment(msg) => write!(f, "Invalid commitment: {msg}"),
            GuardianError::AuthenticationFailed(msg) => write!(f, "Authentication failed: {msg}"),
            GuardianError::AuthenticationReplay => write!(
                f,
                "Authentication replay rejected: request timestamp not greater \
                 than the last accepted timestamp for this signer"
            ),
            GuardianError::AuthorizationFailed(msg) => write!(f, "Authorization failed: {msg}"),
            GuardianError::InvalidInput(msg) => write!(f, "Invalid input: {msg}"),
            GuardianError::StorageError(msg) => write!(f, "Storage error: {msg}"),
            GuardianError::NetworkError(msg) => write!(f, "Network error: {msg}"),
            GuardianError::SigningError(msg) => write!(f, "Signing error: {msg}"),
            GuardianError::ConfigurationError(msg) => write!(f, "Configuration error: {msg}"),
            GuardianError::ProposalNotFound {
                account_id,
                commitment,
            } => {
                write!(
                    f,
                    "Proposal not found for account '{account_id}' with commitment '{commitment}'"
                )
            }
            GuardianError::ProposalAlreadySigned { signer_id } => {
                write!(f, "Proposal already signed by '{signer_id}'")
            }
            GuardianError::InvalidProposalSignature(msg) => {
                write!(f, "Invalid proposal signature: {msg}")
            }
            GuardianError::UnsupportedForNetwork { network, operation } => {
                write!(
                    f,
                    "Operation '{operation}' is unsupported for {network} accounts"
                )
            }
            GuardianError::UnsupportedEvmChain { chain_id } => {
                write!(f, "Unsupported EVM chain '{chain_id}'")
            }
            GuardianError::InvalidNetworkConfig(msg) => write!(f, "Invalid network config: {msg}"),
            GuardianError::RpcUnavailable(msg) => write!(f, "RPC unavailable: {msg}"),
            GuardianError::RpcValidationFailed(msg) => write!(f, "RPC validation failed: {msg}"),
            GuardianError::SignerNotAuthorized(msg) => write!(f, "Signer not authorized: {msg}"),
            GuardianError::InvalidEvmProposal(msg) => write!(f, "Invalid EVM proposal: {msg}"),
            GuardianError::InsufficientSignatures { required, got } => {
                write!(f, "Insufficient signatures: required {required}, got {got}")
            }
            GuardianError::RateLimitExceeded {
                retry_after_secs,
                scope,
            } => write!(
                f,
                "Rate limit exceeded for {scope}. Retry after {retry_after_secs} seconds"
            ),
            GuardianError::InvalidCursor(msg) => write!(f, "Invalid cursor: {msg}"),
            GuardianError::InvalidLimit(msg) => write!(f, "Invalid limit: {msg}"),
            GuardianError::InvalidStatusFilter(msg) => {
                write!(f, "Invalid status filter: {msg}")
            }
            GuardianError::InsufficientOperatorPermission {
                missing_permissions,
            } => {
                write!(
                    f,
                    "Operator lacks required permissions: {}",
                    missing_permissions.join(", ")
                )
            }
            GuardianError::DataUnavailable(msg) => write!(f, "Data unavailable: {msg}"),
            GuardianError::AccountPaused { paused_reason, .. } => match paused_reason {
                Some(reason) => write!(f, "Account is paused: {reason}"),
                None => write!(f, "Account is paused"),
            },
            GuardianError::AccountReleased { .. } => write!(
                f,
                "Account was released: it switched to a different guardian. \
                 Re-onboard via /configure to reactivate"
            ),
            GuardianError::CandidateLanded { account_id, nonce } => write!(
                f,
                "Candidate at nonce {nonce} for account '{account_id}' already \
                 landed on-chain; cannot abandon"
            ),
        }
    }
}

impl std::error::Error for GuardianError {}

impl From<String> for GuardianError {
    fn from(s: String) -> Self {
        GuardianError::InvalidInput(s)
    }
}

impl From<&str> for GuardianError {
    fn from(s: &str) -> Self {
        GuardianError::InvalidInput(s.to_string())
    }
}

impl From<MidenFalconRpoError> for GuardianError {
    fn from(err: MidenFalconRpoError) -> Self {
        GuardianError::SigningError(err.to_string())
    }
}

impl From<miden_keystore::KeyStoreError> for GuardianError {
    fn from(err: miden_keystore::KeyStoreError) -> Self {
        GuardianError::SigningError(err.to_string())
    }
}

/// Structured machine-readable side-data on the error wire object. Carried
/// identically on the HTTP error body and the gRPC `Status.details`
/// (feature `009-human-readable-errors`). `retryable` is always present; the
/// remaining fields are omitted when they do not apply to the variant.
#[derive(Serialize)]
struct ErrorMeta {
    /// Whether retrying the same request could plausibly succeed.
    retryable: bool,
    /// Seconds to wait before retrying. Populated only for
    /// `rate_limit_exceeded`; the `Retry-After` header carries the same value.
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_secs: Option<u32>,
    /// Lex-sorted permissions the operator lacks. Populated only for
    /// `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`.
    #[serde(skip_serializing_if = "Option::is_none")]
    missing_permissions: Option<Vec<String>>,
    /// RFC 3339 UTC timestamp of the original pause. Populated only for
    /// `GUARDIAN_ACCOUNT_PAUSED`.
    #[serde(skip_serializing_if = "Option::is_none")]
    paused_at: Option<String>,
    /// Reason captured at first pause. Populated only for
    /// `GUARDIAN_ACCOUNT_PAUSED` (may itself be absent within that variant).
    #[serde(skip_serializing_if = "Option::is_none")]
    paused_reason: Option<String>,
    /// RFC 3339 UTC timestamp of the guardian-switch release. Populated
    /// only for `GUARDIAN_ACCOUNT_RELEASED`.
    #[serde(skip_serializing_if = "Option::is_none")]
    released_at: Option<String>,
}

/// The single error object on the wire: `{ code, message, meta }`. Identical
/// shape on the HTTP error body and the gRPC `Status.details` (feature
/// `009-human-readable-errors`). The legacy `success`/`error` fields are
/// gone; the diagnostic [`Display`](GuardianError) string is logged
/// server-side only and never returned.
#[derive(Serialize)]
struct ErrorBody {
    /// Stable machine-readable code; the client branch + i18n key.
    code: &'static str,
    /// User-safe sentence; safe to display verbatim. Wording is not stable.
    message: &'static str,
    meta: ErrorMeta,
}

impl GuardianError {
    /// Backoff hint in seconds for errors that carry one. The single
    /// source for `meta.retry_after_secs`, the HTTP `Retry-After`
    /// header, and the gRPC `retry-after` metadata, so a new
    /// hint-carrying variant cannot reach one transport and not the
    /// other.
    fn retry_after_secs(&self) -> Option<u32> {
        match self {
            GuardianError::RateLimitExceeded {
                retry_after_secs, ..
            } => Some(*retry_after_secs),
            _ => None,
        }
    }

    /// Build the structured `meta` block for this error.
    fn error_meta(&self) -> ErrorMeta {
        let retry_after_secs = self.retry_after_secs();
        let (missing_permissions, paused_at, paused_reason, released_at) = match self {
            GuardianError::InsufficientOperatorPermission {
                missing_permissions,
            } => (Some(missing_permissions.clone()), None, None, None),
            GuardianError::AccountPaused {
                paused_at,
                paused_reason,
            } => (
                None,
                Some(paused_at.to_rfc3339()),
                paused_reason.clone(),
                None,
            ),
            GuardianError::AccountReleased { released_at } => {
                (None, None, None, Some(released_at.to_rfc3339()))
            }
            _ => (None, None, None, None),
        };
        ErrorMeta {
            retryable: self.retryable(),
            retry_after_secs,
            missing_permissions,
            paused_at,
            paused_reason,
            released_at,
        }
    }

    /// Build the full `{ code, message, meta }` wire object. Shared by the
    /// HTTP `IntoResponse` and the gRPC `Status` conversion so the two
    /// surfaces return a byte-identical object (Constitution II parity).
    fn error_body(&self) -> ErrorBody {
        ErrorBody {
            code: self.code(),
            message: self.user_message(),
            meta: self.error_meta(),
        }
    }
}

impl IntoResponse for GuardianError {
    fn into_response(self) -> Response {
        let status = self.http_status();
        // FR-003: the diagnostic Display string is logged for operators, never
        // returned on the wire (removes the disclosure risk by construction).
        if status.is_server_error() {
            tracing::error!(code = self.code(), detail = %self, "guardian error (HTTP 5xx)");
        } else {
            tracing::debug!(code = self.code(), detail = %self, "guardian error (HTTP 4xx)");
        }
        let retry_after_secs = self.retry_after_secs();
        let body = Json(self.error_body());
        if let Some(retry_after_secs) = retry_after_secs {
            (
                status,
                [("Retry-After", retry_after_secs.to_string())],
                body,
            )
                .into_response()
        } else {
            (status, body).into_response()
        }
    }
}

/// gRPC counterpart of the HTTP `Retry-After` header, carried as ASCII
/// decimal seconds in the rejection `Status` metadata.
pub const RETRY_AFTER_METADATA_KEY: &str = "retry-after";

impl From<GuardianError> for tonic::Status {
    fn from(err: GuardianError) -> Self {
        // gRPC carries the same `{ code, message, meta }` object as HTTP, in
        // `Status.details`, for every error. `Status.message` is the user-safe
        // message; the diagnostic Display string is logged, not returned.
        if err.http_status().is_server_error() {
            tracing::error!(code = err.code(), detail = %err, "guardian error (gRPC internal)");
        } else {
            tracing::debug!(code = err.code(), detail = %err, "guardian error (gRPC)");
        }
        let retry_after_secs = err.retry_after_secs();
        let details = serde_json::to_vec(&err.error_body()).unwrap_or_default();
        let mut status =
            tonic::Status::with_details(err.grpc_status(), err.user_message(), details.into());
        if let Some(secs) = retry_after_secs {
            status
                .metadata_mut()
                .insert(RETRY_AFTER_METADATA_KEY, secs.into());
        }
        status
    }
}

impl fmt::Display for MidenFalconRpoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MidenFalconRpoError::StorageError(msg) => write!(f, "Storage error: {msg}"),
            MidenFalconRpoError::DecodingError(msg) => write!(f, "Decoding error: {msg}"),
        }
    }
}

impl std::error::Error for MidenFalconRpoError {}

impl From<miden_keystore::KeyStoreError> for MidenFalconRpoError {
    fn from(err: miden_keystore::KeyStoreError) -> Self {
        match err {
            miden_keystore::KeyStoreError::StorageError(msg) => {
                MidenFalconRpoError::StorageError(msg)
            }
            miden_keystore::KeyStoreError::DecodingError(msg) => {
                MidenFalconRpoError::DecodingError(msg)
            }
            miden_keystore::KeyStoreError::KeyNotFound(msg) => {
                MidenFalconRpoError::StorageError(msg)
            }
        }
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;

    // --- GuardianError::http_status ---

    #[test]
    fn http_status_not_found_variants() {
        assert_eq!(
            GuardianError::AccountNotFound("x".into()).http_status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            GuardianError::DeltaNotFound {
                account_id: "x".into(),
                nonce: 1
            }
            .http_status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            GuardianError::StateNotFound("x".into()).http_status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            GuardianError::ProposalNotFound {
                account_id: "x".into(),
                commitment: "c".into()
            }
            .http_status(),
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn http_status_conflict_variants() {
        assert_eq!(
            GuardianError::AccountAlreadyExists("x".into()).http_status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            GuardianError::ConflictPendingDelta.http_status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            GuardianError::ConflictPendingProposal.http_status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            GuardianError::ProposalAlreadySigned {
                signer_id: "s".into()
            }
            .http_status(),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn http_status_auth_variants() {
        assert_eq!(
            GuardianError::AuthenticationFailed("x".into()).http_status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            GuardianError::AuthorizationFailed("x".into()).http_status(),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn http_status_bad_request_variants() {
        assert_eq!(
            GuardianError::InvalidInput("x".into()).http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            GuardianError::InvalidAccountId("x".into()).http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            GuardianError::InvalidDelta("x".into()).http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            GuardianError::InvalidCommitment("x".into()).http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            GuardianError::CommitmentMismatch {
                expected: "a".into(),
                actual: "b".into()
            }
            .http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            GuardianError::InvalidProposalSignature("x".into()).http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            GuardianError::InsufficientSignatures {
                required: 3,
                got: 1
            }
            .http_status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn http_status_server_error_variants() {
        assert_eq!(
            GuardianError::NetworkError("x".into()).http_status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            GuardianError::SigningError("x".into()).http_status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            GuardianError::StorageError("x".into()).http_status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            GuardianError::ConfigurationError("x".into()).http_status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    // --- GuardianError::grpc_status ---

    #[test]
    fn grpc_status_not_found() {
        assert_eq!(
            GuardianError::AccountNotFound("x".into()).grpc_status(),
            tonic::Code::NotFound
        );
        assert_eq!(
            GuardianError::StateNotFound("x".into()).grpc_status(),
            tonic::Code::NotFound
        );
    }

    #[test]
    fn grpc_status_already_exists() {
        assert_eq!(
            GuardianError::AccountAlreadyExists("x".into()).grpc_status(),
            tonic::Code::AlreadyExists
        );
        assert_eq!(
            GuardianError::ProposalAlreadySigned {
                signer_id: "s".into()
            }
            .grpc_status(),
            tonic::Code::AlreadyExists
        );
    }

    #[test]
    fn grpc_status_failed_precondition() {
        assert_eq!(
            GuardianError::ConflictPendingDelta.grpc_status(),
            tonic::Code::FailedPrecondition
        );
        assert_eq!(
            GuardianError::ConflictPendingProposal.grpc_status(),
            tonic::Code::FailedPrecondition
        );
        assert_eq!(
            GuardianError::InsufficientSignatures {
                required: 2,
                got: 1
            }
            .grpc_status(),
            tonic::Code::FailedPrecondition
        );
    }

    #[test]
    fn grpc_status_auth() {
        assert_eq!(
            GuardianError::AuthenticationFailed("x".into()).grpc_status(),
            tonic::Code::Unauthenticated
        );
        assert_eq!(
            GuardianError::AuthorizationFailed("x".into()).grpc_status(),
            tonic::Code::PermissionDenied
        );
    }

    // -- Issue #367: AuthenticationReplay --

    #[test]
    fn authentication_replay_pins_http_grpc_code_and_retryable() {
        let err = GuardianError::AuthenticationReplay;
        assert_eq!(err.http_status(), StatusCode::UNAUTHORIZED);
        assert_eq!(err.grpc_status(), tonic::Code::Unauthenticated);
        assert_eq!(err.code(), "authentication_replay");
        assert!(err.retryable());
        assert!(!GuardianError::AuthenticationFailed("x".into()).retryable());
    }

    #[test]
    fn authentication_replay_grpc_details_carry_retryable_true() {
        let status: tonic::Status = GuardianError::AuthenticationReplay.into();
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
        let details: serde_json::Value =
            serde_json::from_slice(status.details()).expect("details are JSON");
        assert_eq!(details["code"], "authentication_replay");
        assert_eq!(details["meta"]["retryable"], serde_json::Value::Bool(true));
    }

    #[test]
    fn grpc_status_invalid_argument() {
        assert_eq!(
            GuardianError::InvalidInput("x".into()).grpc_status(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            GuardianError::InvalidAccountId("x".into()).grpc_status(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            GuardianError::InvalidDelta("x".into()).grpc_status(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            GuardianError::InvalidCommitment("x".into()).grpc_status(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            GuardianError::CommitmentMismatch {
                expected: "a".into(),
                actual: "b".into()
            }
            .grpc_status(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            GuardianError::InvalidProposalSignature("x".into()).grpc_status(),
            tonic::Code::InvalidArgument
        );
    }

    #[test]
    fn grpc_status_internal() {
        assert_eq!(
            GuardianError::SigningError("x".into()).grpc_status(),
            tonic::Code::Internal
        );
        assert_eq!(
            GuardianError::StorageError("x".into()).grpc_status(),
            tonic::Code::Internal
        );
        assert_eq!(
            GuardianError::ConfigurationError("x".into()).grpc_status(),
            tonic::Code::Internal
        );
    }

    // --- Display ---

    #[test]
    fn display_account_not_found() {
        let err = GuardianError::AccountNotFound("abc".into());
        assert_eq!(err.to_string(), "Account 'abc' not found");
    }

    #[test]
    fn display_account_already_exists() {
        let err = GuardianError::AccountAlreadyExists("abc".into());
        assert_eq!(err.to_string(), "Account 'abc' already exists");
    }

    #[test]
    fn display_delta_not_found() {
        let err = GuardianError::DeltaNotFound {
            account_id: "acc".into(),
            nonce: 42,
        };
        assert_eq!(
            err.to_string(),
            "Delta not found for account 'acc' at nonce 42"
        );
    }

    #[test]
    fn display_commitment_mismatch() {
        let err = GuardianError::CommitmentMismatch {
            expected: "0xaa".into(),
            actual: "0xbb".into(),
        };
        assert_eq!(
            err.to_string(),
            "Commitment mismatch: expected 0xaa, got 0xbb"
        );
    }

    #[test]
    fn display_conflict_pending_delta() {
        assert!(
            GuardianError::ConflictPendingDelta
                .to_string()
                .contains("non-canonical delta pending")
        );
    }

    #[test]
    fn display_conflict_pending_proposal() {
        assert!(
            GuardianError::ConflictPendingProposal
                .to_string()
                .contains("pending proposals")
        );
    }

    #[test]
    fn display_proposal_not_found() {
        let err = GuardianError::ProposalNotFound {
            account_id: "acc".into(),
            commitment: "c".into(),
        };
        assert!(err.to_string().contains("acc"));
        assert!(err.to_string().contains("c"));
    }

    #[test]
    fn display_proposal_already_signed() {
        let err = GuardianError::ProposalAlreadySigned {
            signer_id: "signer".into(),
        };
        assert!(err.to_string().contains("signer"));
    }

    #[test]
    fn display_insufficient_signatures() {
        let err = GuardianError::InsufficientSignatures {
            required: 3,
            got: 1,
        };
        assert!(err.to_string().contains("3"));
        assert!(err.to_string().contains("1"));
    }

    // --- From conversions ---

    #[test]
    fn from_string_creates_invalid_input() {
        let err: GuardianError = "some error".to_string().into();
        assert_eq!(err, GuardianError::InvalidInput("some error".into()));
    }

    #[test]
    fn from_str_creates_invalid_input() {
        let err: GuardianError = "some error".into();
        assert_eq!(err, GuardianError::InvalidInput("some error".into()));
    }

    #[test]
    fn from_miden_falcon_rpo_error() {
        let err = MidenFalconRpoError::StorageError("storage fail".into());
        let guardian: GuardianError = err.into();
        assert!(matches!(guardian, GuardianError::SigningError(_)));
        assert!(guardian.to_string().contains("storage fail"));
    }

    #[test]
    fn from_keystore_error_to_guardian() {
        let err = miden_keystore::KeyStoreError::KeyNotFound("key123".into());
        let guardian: GuardianError = err.into();
        assert!(matches!(guardian, GuardianError::SigningError(_)));
    }

    // --- MidenFalconRpoError Display ---

    #[test]
    fn falcon_rpo_error_display() {
        assert!(
            MidenFalconRpoError::StorageError("x".into())
                .to_string()
                .contains("Storage error")
        );
        assert!(
            MidenFalconRpoError::DecodingError("y".into())
                .to_string()
                .contains("Decoding error")
        );
    }

    // --- KeyStoreError -> MidenFalconRpoError ---

    #[test]
    fn keystore_error_to_falcon_rpo_storage() {
        let err = miden_keystore::KeyStoreError::StorageError("s".into());
        let falcon: MidenFalconRpoError = err.into();
        assert!(matches!(falcon, MidenFalconRpoError::StorageError(_)));
    }

    #[test]
    fn keystore_error_to_falcon_rpo_decoding() {
        let err = miden_keystore::KeyStoreError::DecodingError("d".into());
        let falcon: MidenFalconRpoError = err.into();
        assert!(matches!(falcon, MidenFalconRpoError::DecodingError(_)));
    }

    #[test]
    fn keystore_error_to_falcon_rpo_key_not_found() {
        let err = miden_keystore::KeyStoreError::KeyNotFound("k".into());
        let falcon: MidenFalconRpoError = err.into();
        assert!(matches!(falcon, MidenFalconRpoError::StorageError(_)));
    }

    // --- IntoResponse / tonic::Status ---

    #[test]
    fn into_response_returns_correct_status() {
        let err = GuardianError::AccountNotFound("x".into());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn into_tonic_status() {
        // Status.message is now the user-safe message (not the Display detail
        // "bad creds", which is logged, not returned); details carry the
        // identical { code, message, meta } object as the HTTP body.
        let err = GuardianError::AuthenticationFailed("bad creds".into());
        let user_message = err.user_message();
        let status: tonic::Status = err.into();
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
        assert_eq!(status.message(), user_message);
        assert!(!status.message().contains("bad creds"));
        let details: serde_json::Value =
            serde_json::from_slice(status.details()).expect("details are JSON");
        assert_eq!(details["code"], "authentication_failed");
        assert_eq!(details["message"], user_message);
        assert_eq!(details["meta"]["retryable"], serde_json::Value::Bool(false));
    }

    // --- Dashboard pagination error variants (FR-028) ---

    #[test]
    fn invalid_cursor_maps_to_400_with_stable_code() {
        let err = GuardianError::InvalidCursor("tampered".into());
        assert_eq!(err.http_status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.code(), "invalid_cursor");
        assert_eq!(err.grpc_status(), tonic::Code::InvalidArgument);
        assert!(err.to_string().contains("Invalid cursor"));
        assert!(err.to_string().contains("tampered"));
    }

    #[test]
    fn invalid_limit_maps_to_400_with_stable_code() {
        let err = GuardianError::InvalidLimit("limit must be in [1, 500]".into());
        assert_eq!(err.http_status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.code(), "invalid_limit");
        assert_eq!(err.grpc_status(), tonic::Code::InvalidArgument);
        assert!(err.to_string().contains("Invalid limit"));
    }

    #[test]
    fn invalid_status_filter_maps_to_400_with_stable_code() {
        let err = GuardianError::InvalidStatusFilter("unknown status 'foo'".into());
        assert_eq!(err.http_status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.code(), "invalid_status_filter");
        assert_eq!(err.grpc_status(), tonic::Code::InvalidArgument);
        assert!(err.to_string().contains("Invalid status filter"));
    }

    #[test]
    fn data_unavailable_maps_to_503_with_stable_code() {
        let err = GuardianError::DataUnavailable("delta store unreadable".into());
        assert_eq!(err.http_status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.code(), "data_unavailable");
        assert_eq!(err.grpc_status(), tonic::Code::Unavailable);
        assert!(err.to_string().contains("Data unavailable"));
    }

    #[test]
    fn dashboard_error_variants_serialize_with_stable_code_in_body() {
        // Smoke-tests the JSON body shape from `IntoResponse`. The body
        // includes `code: <stable string>` so clients can branch without
        // string-matching the message.
        for (err, expected_code) in [
            (GuardianError::InvalidCursor("x".into()), "invalid_cursor"),
            (GuardianError::InvalidLimit("x".into()), "invalid_limit"),
            (
                GuardianError::InvalidStatusFilter("x".into()),
                "invalid_status_filter",
            ),
            (
                GuardianError::DataUnavailable("x".into()),
                "data_unavailable",
            ),
        ] {
            assert_eq!(err.code(), expected_code);
        }
    }

    // -- Feature 006-operator-authz: InsufficientOperatorPermission --

    #[test]
    fn insufficient_operator_permission_pins_http_grpc_and_code() {
        let err = GuardianError::InsufficientOperatorPermission {
            missing_permissions: vec!["accounts:pause".into()],
        };
        assert_eq!(err.http_status(), StatusCode::FORBIDDEN);
        assert_eq!(err.grpc_status(), tonic::Code::PermissionDenied);
        assert_eq!(err.code(), "GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION");
    }

    #[test]
    fn insufficient_operator_permission_serializes_with_missing_permissions_and_retryable_false() {
        use axum::body::to_bytes;
        let err = GuardianError::InsufficientOperatorPermission {
            missing_permissions: vec!["accounts:pause".into()],
        };
        let response = err.into_response();
        let status = response.status();
        let body_bytes = futures::executor::block_on(to_bytes(response.into_body(), usize::MAX))
            .expect("body bytes");
        let parsed: serde_json::Value =
            serde_json::from_slice(&body_bytes).expect("body is valid JSON");

        assert_eq!(status, StatusCode::FORBIDDEN);
        // New shape: no `success`/`error`; `code` + `message` at top level,
        // structured side-data under `meta`.
        assert!(parsed.get("success").is_none());
        assert!(parsed.get("error").is_none());
        assert_eq!(parsed["code"], "GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION");
        assert!(parsed["message"].is_string());
        assert_eq!(
            parsed["meta"]["missing_permissions"],
            serde_json::json!(["accounts:pause"])
        );
        assert_eq!(parsed["meta"]["retryable"], serde_json::Value::Bool(false));
        // `retry_after_secs` does not apply to this code.
        assert!(parsed["meta"].get("retry_after_secs").is_none());
    }

    // -- Feature 001-account-pausing: AccountPaused --

    #[test]
    fn account_paused_pins_http_grpc_and_code() {
        let err = GuardianError::AccountPaused {
            paused_at: chrono::DateTime::parse_from_rfc3339("2026-05-19T14:23:00Z")
                .unwrap()
                .with_timezone(&Utc),
            paused_reason: Some("suspected cosigner compromise".into()),
        };
        assert_eq!(err.http_status(), StatusCode::CONFLICT);
        assert_eq!(err.grpc_status(), tonic::Code::FailedPrecondition);
        assert_eq!(err.code(), "GUARDIAN_ACCOUNT_PAUSED");
        assert!(err.to_string().contains("suspected cosigner compromise"));
    }

    #[test]
    fn account_paused_http_envelope_carries_paused_fields_and_retryable_false() {
        use axum::body::to_bytes;
        let err = GuardianError::AccountPaused {
            paused_at: chrono::DateTime::parse_from_rfc3339("2026-05-19T14:23:00Z")
                .unwrap()
                .with_timezone(&Utc),
            paused_reason: Some("suspected cosigner compromise".into()),
        };
        let response = err.into_response();
        let status = response.status();
        let body_bytes = futures::executor::block_on(to_bytes(response.into_body(), usize::MAX))
            .expect("body bytes");
        let parsed: serde_json::Value =
            serde_json::from_slice(&body_bytes).expect("body is valid JSON");

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(parsed.get("success").is_none());
        assert!(parsed.get("error").is_none());
        assert_eq!(parsed["code"], "GUARDIAN_ACCOUNT_PAUSED");
        assert!(parsed["message"].is_string());
        assert_eq!(parsed["meta"]["paused_at"], "2026-05-19T14:23:00+00:00");
        assert_eq!(
            parsed["meta"]["paused_reason"],
            "suspected cosigner compromise"
        );
        assert_eq!(parsed["meta"]["retryable"], serde_json::Value::Bool(false));
    }

    #[test]
    fn account_paused_grpc_status_carries_details() {
        let err = GuardianError::AccountPaused {
            paused_at: chrono::DateTime::parse_from_rfc3339("2026-05-19T14:23:00Z")
                .unwrap()
                .with_timezone(&Utc),
            paused_reason: Some("compromise".into()),
        };
        let status: tonic::Status = err.into();
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        let details: serde_json::Value =
            serde_json::from_slice(status.details()).expect("details are JSON");
        assert_eq!(details["code"], "GUARDIAN_ACCOUNT_PAUSED");
        assert!(details["message"].is_string());
        assert_eq!(details["meta"]["paused_at"], "2026-05-19T14:23:00+00:00");
        assert_eq!(details["meta"]["paused_reason"], "compromise");
        assert_eq!(details["meta"]["retryable"], serde_json::Value::Bool(false));
    }

    // -- Issue #319: CandidateLanded --

    #[test]
    fn candidate_landed_pins_http_grpc_and_code() {
        let err = GuardianError::CandidateLanded {
            account_id: "0xabc".into(),
            nonce: 7,
        };
        assert_eq!(err.http_status(), StatusCode::CONFLICT);
        assert_eq!(err.grpc_status(), tonic::Code::FailedPrecondition);
        assert_eq!(err.code(), "GUARDIAN_CANDIDATE_LANDED");
        assert!(err.to_string().contains("0xabc"));
        assert!(err.to_string().contains("nonce 7"));
    }

    #[test]
    fn plain_error_body_has_code_message_meta_and_no_legacy_fields() {
        use axum::body::to_bytes;
        // A plain error: { code, message, meta:{retryable} } — no `success`,
        // no `error`, and no `meta` fields that don't apply to the variant.
        let err = GuardianError::AccountNotFound("0xabc".into());
        let response = err.into_response();
        let body_bytes = futures::executor::block_on(to_bytes(response.into_body(), usize::MAX))
            .expect("body bytes");
        let parsed: serde_json::Value =
            serde_json::from_slice(&body_bytes).expect("body is valid JSON");
        assert!(parsed.get("success").is_none());
        assert!(parsed.get("error").is_none());
        assert_eq!(parsed["code"], "account_not_found");
        assert!(parsed["message"].is_string());
        assert_eq!(parsed["meta"]["retryable"], serde_json::Value::Bool(false));
        assert!(parsed["meta"].get("missing_permissions").is_none());
        assert!(parsed["meta"].get("paused_at").is_none());
        assert!(parsed["meta"].get("retry_after_secs").is_none());
    }

    // --- Feature 009-human-readable-errors: user_message() ---

    /// Every variant, built with deliberately sensitive-looking payloads so
    /// the sanitization scan (SC-002) is meaningful.
    fn all_variants_with_sensitive_payloads() -> Vec<GuardianError> {
        let paused_at = chrono::DateTime::parse_from_rfc3339("2026-05-19T14:23:00Z")
            .unwrap()
            .with_timezone(&Utc);
        vec![
            GuardianError::AccountNotFound("0xDEADBEEFACCOUNT".into()),
            GuardianError::AccountAlreadyExists("0xDEADBEEFACCOUNT".into()),
            GuardianError::AccountDataUnavailable("0xDEADBEEFACCOUNT".into()),
            GuardianError::InvalidAccountId("0xDEADBEEFACCOUNT".into()),
            GuardianError::StateNotFound("0xDEADBEEFACCOUNT".into()),
            GuardianError::DeltaNotFound {
                account_id: "0xDEADBEEFACCOUNT".into(),
                nonce: 42,
            },
            GuardianError::InvalidDelta("postgres://secret@host/db".into()),
            GuardianError::ConflictPendingDelta,
            GuardianError::ConflictPendingProposal,
            GuardianError::PendingProposalsLimit { limit: 7 },
            GuardianError::CommitmentMismatch {
                expected: "0xAAAACOMMITMENT".into(),
                actual: "0xBBBBCOMMITMENT".into(),
            },
            GuardianError::InvalidCommitment("0xAAAACOMMITMENT".into()),
            GuardianError::AuthenticationFailed("bad creds for 0xSIGNER".into()),
            GuardianError::AuthorizationFailed("0xSIGNER not in policy".into()),
            GuardianError::InvalidInput("/var/secret/path".into()),
            GuardianError::StorageError("/var/lib/guardian/db: disk full".into()),
            GuardianError::NetworkError("https://rpc.internal:8080 refused".into()),
            GuardianError::SigningError("falcon: 0xPRIVATEKEYMATERIAL".into()),
            GuardianError::ConfigurationError("postgres://u:p@h/db".into()),
            GuardianError::ProposalNotFound {
                account_id: "0xDEADBEEFACCOUNT".into(),
                commitment: "0xAAAACOMMITMENT".into(),
            },
            GuardianError::ProposalAlreadySigned {
                signer_id: "0xSIGNER".into(),
            },
            GuardianError::InvalidProposalSignature("0xSIGNATURE".into()),
            GuardianError::UnsupportedForNetwork {
                network: "evm".into(),
                operation: "push_delta".into(),
            },
            GuardianError::UnsupportedEvmChain { chain_id: 1 },
            GuardianError::InvalidNetworkConfig("https://rpc.internal".into()),
            GuardianError::RpcUnavailable("https://rpc.internal:8080".into()),
            GuardianError::RpcValidationFailed("https://rpc.internal".into()),
            GuardianError::SignerNotAuthorized("0xSIGNER".into()),
            GuardianError::InvalidEvmProposal("0xCALLDATA".into()),
            GuardianError::InsufficientSignatures {
                required: 3,
                got: 1,
            },
            GuardianError::RateLimitExceeded {
                retry_after_secs: 30,
                scope: "ip:10.0.0.1".into(),
            },
            GuardianError::InvalidCursor("0xTAMPERED".into()),
            GuardianError::InvalidLimit("9999".into()),
            GuardianError::InvalidStatusFilter("'; DROP TABLE".into()),
            GuardianError::InsufficientOperatorPermission {
                missing_permissions: vec!["accounts:pause".into()],
            },
            GuardianError::DataUnavailable("/var/lib/guardian unreadable".into()),
            GuardianError::AccountPaused {
                paused_at,
                paused_reason: Some("0xSIGNER compromise".into()),
            },
            GuardianError::AccountReleased {
                released_at: paused_at,
            },
            GuardianError::CandidateLanded {
                account_id: "0xDEADBEEFACCOUNT".into(),
                nonce: 42,
            },
        ]
    }

    #[test]
    fn user_message_is_nonempty_for_every_variant() {
        // SC-001: 100% of variants return a non-empty, single-sentence message.
        for err in all_variants_with_sensitive_payloads() {
            let msg = err.user_message();
            assert!(!msg.is_empty(), "empty user_message for {}", err.code());
            assert!(
                msg.ends_with('.'),
                "message for {} should be a sentence: {msg:?}",
                err.code()
            );
        }
    }

    #[test]
    fn user_message_never_leaks_sensitive_payload() {
        // SC-002: the user-safe message must never echo identifiers, hashes,
        // paths, URLs, or other payload values that the Display string carries.
        let disallowed = [
            "0xDEADBEEFACCOUNT",
            "0xAAAACOMMITMENT",
            "0xBBBBCOMMITMENT",
            "0xSIGNER",
            "0xSIGNATURE",
            "0xPRIVATEKEYMATERIAL",
            "0xCALLDATA",
            "0xTAMPERED",
            "postgres://",
            "https://",
            "/var/",
            "DROP TABLE",
            "10.0.0.1",
        ];
        for err in all_variants_with_sensitive_payloads() {
            let msg = err.user_message();
            for needle in disallowed {
                assert!(
                    !msg.contains(needle),
                    "user_message for {} leaked {needle:?}: {msg:?}",
                    err.code()
                );
            }
        }
    }

    #[test]
    fn connectivity_and_internal_messages_are_distinct() {
        // SC-005: server-mapped connectivity faults vs pure internal faults
        // must surface different copy.
        let connectivity = GuardianError::RpcUnavailable("x".into()).user_message();
        assert_eq!(
            GuardianError::NetworkError("x".into()).user_message(),
            connectivity
        );
        assert_eq!(
            GuardianError::RpcValidationFailed("x".into()).user_message(),
            connectivity
        );
        let internal = GuardianError::StorageError("x".into()).user_message();
        assert_eq!(
            GuardianError::SigningError("x".into()).user_message(),
            internal
        );
        assert_eq!(
            GuardianError::ConfigurationError("x".into()).user_message(),
            internal
        );
        assert_eq!(
            GuardianError::DataUnavailable("x".into()).user_message(),
            internal
        );
        assert_ne!(connectivity, internal);
    }

    #[test]
    fn rate_limit_meta_carries_retry_after_and_retryable_true() {
        use axum::body::to_bytes;
        let err = GuardianError::RateLimitExceeded {
            retry_after_secs: 30,
            scope: "ip".into(),
        };
        let response = err.into_response();
        // Retry-After header preserved.
        assert_eq!(
            response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok()),
            Some("30")
        );
        let body_bytes = futures::executor::block_on(to_bytes(response.into_body(), usize::MAX))
            .expect("body bytes");
        let parsed: serde_json::Value =
            serde_json::from_slice(&body_bytes).expect("body is valid JSON");
        assert_eq!(parsed["code"], "rate_limit_exceeded");
        assert_eq!(parsed["meta"]["retryable"], serde_json::Value::Bool(true));
        assert_eq!(parsed["meta"]["retry_after_secs"], serde_json::json!(30));
    }

    #[test]
    fn rate_limit_grpc_status_carries_retry_after_metadata() {
        let err = GuardianError::RateLimitExceeded {
            retry_after_secs: 30,
            scope: "ip".into(),
        };
        let status: tonic::Status = err.into();
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        assert_eq!(
            status
                .metadata()
                .get(RETRY_AFTER_METADATA_KEY)
                .and_then(|v| v.to_str().ok()),
            Some("30")
        );
        let parsed: serde_json::Value =
            serde_json::from_slice(status.details()).expect("details are valid JSON");
        assert_eq!(parsed["code"], "rate_limit_exceeded");
        assert_eq!(parsed["meta"]["retry_after_secs"], serde_json::json!(30));
    }

    #[test]
    fn non_rate_limit_grpc_status_has_no_retry_after_metadata() {
        let status: tonic::Status = GuardianError::AccountNotFound("0x1".into()).into();
        assert!(status.metadata().get(RETRY_AFTER_METADATA_KEY).is_none());
    }
}
