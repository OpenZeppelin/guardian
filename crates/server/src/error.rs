use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use std::fmt;

/// Primary error type for PSM operations
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PsmError {
    AccountNotFound(String),
    AccountAlreadyExists(String),
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
    InsufficientSignatures {
        required: usize,
        got: usize,
    },
}

/// Signing-specific error type for Miden Falcon RPO operations
#[derive(Debug)]
pub enum MidenFalconRpoError {
    StorageError(String),
    DecodingError(String),
}

/// Result type alias for PSM operations
pub type Result<T> = std::result::Result<T, PsmError>;

/// Result type alias for Miden Falcon RPO signing operations
pub type MidenFalconRpoResult<T> = std::result::Result<T, MidenFalconRpoError>;

impl PsmError {
    pub fn http_status(&self) -> StatusCode {
        match self {
            PsmError::AccountNotFound(_) => StatusCode::NOT_FOUND,
            PsmError::DeltaNotFound { .. } => StatusCode::NOT_FOUND,
            PsmError::StateNotFound(_) => StatusCode::NOT_FOUND,
            PsmError::ProposalNotFound { .. } => StatusCode::NOT_FOUND,
            PsmError::AccountAlreadyExists(_) => StatusCode::CONFLICT,
            PsmError::ConflictPendingDelta => StatusCode::CONFLICT,
            PsmError::ConflictPendingProposal => StatusCode::CONFLICT,
            PsmError::PendingProposalsLimit { .. } => StatusCode::CONFLICT,
            PsmError::ProposalAlreadySigned { .. } => StatusCode::CONFLICT,
            PsmError::AuthenticationFailed(_) => StatusCode::UNAUTHORIZED,
            PsmError::AuthorizationFailed(_) => StatusCode::FORBIDDEN,
            PsmError::InvalidInput(_) => StatusCode::BAD_REQUEST,
            PsmError::InvalidAccountId(_) => StatusCode::BAD_REQUEST,
            PsmError::InvalidDelta(_) => StatusCode::BAD_REQUEST,
            PsmError::InvalidCommitment(_) => StatusCode::BAD_REQUEST,
            PsmError::CommitmentMismatch { .. } => StatusCode::BAD_REQUEST,
            PsmError::InvalidProposalSignature(_) => StatusCode::BAD_REQUEST,
            PsmError::InsufficientSignatures { .. } => StatusCode::BAD_REQUEST,
            PsmError::NetworkError(_) => StatusCode::BAD_GATEWAY,
            PsmError::SigningError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            PsmError::StorageError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            PsmError::ConfigurationError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn grpc_status(&self) -> tonic::Code {
        match self {
            PsmError::AccountNotFound(_) => tonic::Code::NotFound,
            PsmError::DeltaNotFound { .. } => tonic::Code::NotFound,
            PsmError::StateNotFound(_) => tonic::Code::NotFound,
            PsmError::ProposalNotFound { .. } => tonic::Code::NotFound,
            PsmError::AccountAlreadyExists(_) => tonic::Code::AlreadyExists,
            PsmError::ConflictPendingDelta => tonic::Code::FailedPrecondition,
            PsmError::ConflictPendingProposal => tonic::Code::FailedPrecondition,
            PsmError::PendingProposalsLimit { .. } => tonic::Code::FailedPrecondition,
            PsmError::ProposalAlreadySigned { .. } => tonic::Code::AlreadyExists,
            PsmError::AuthenticationFailed(_) => tonic::Code::Unauthenticated,
            PsmError::AuthorizationFailed(_) => tonic::Code::PermissionDenied,
            PsmError::InvalidInput(_) => tonic::Code::InvalidArgument,
            PsmError::InvalidAccountId(_) => tonic::Code::InvalidArgument,
            PsmError::InvalidDelta(_) => tonic::Code::InvalidArgument,
            PsmError::InvalidCommitment(_) => tonic::Code::InvalidArgument,
            PsmError::CommitmentMismatch { .. } => tonic::Code::InvalidArgument,
            PsmError::InvalidProposalSignature(_) => tonic::Code::InvalidArgument,
            PsmError::InsufficientSignatures { .. } => tonic::Code::FailedPrecondition,
            PsmError::NetworkError(_) => tonic::Code::Unavailable,
            PsmError::SigningError(_) => tonic::Code::Internal,
            PsmError::StorageError(_) => tonic::Code::Internal,
            PsmError::ConfigurationError(_) => tonic::Code::Internal,
        }
    }
}

impl fmt::Display for PsmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PsmError::AccountNotFound(id) => write!(f, "Account '{id}' not found"),
            PsmError::AccountAlreadyExists(id) => write!(f, "Account '{id}' already exists"),
            PsmError::InvalidAccountId(msg) => write!(f, "Invalid account ID: {msg}"),
            PsmError::StateNotFound(id) => write!(f, "State not found for account '{id}'"),
            PsmError::DeltaNotFound { account_id, nonce } => {
                write!(
                    f,
                    "Delta not found for account '{account_id}' at nonce {nonce}"
                )
            }
            PsmError::InvalidDelta(msg) => write!(f, "Invalid delta: {msg}"),
            PsmError::ConflictPendingDelta => {
                write!(
                    f,
                    "Cannot push new delta: there is already a non-canonical delta pending"
                )
            }
            PsmError::ConflictPendingProposal => {
                write!(f, "Cannot push new delta: there are pending proposals")
            }
            PsmError::PendingProposalsLimit { limit } => write!(
                f,
                "Cannot push new delta proposal: maximum pending proposal limit ({limit}) reached for this account"
            ),
            PsmError::CommitmentMismatch { expected, actual } => {
                write!(f, "Commitment mismatch: expected {expected}, got {actual}")
            }
            PsmError::InvalidCommitment(msg) => write!(f, "Invalid commitment: {msg}"),
            PsmError::AuthenticationFailed(msg) => write!(f, "Authentication failed: {msg}"),
            PsmError::AuthorizationFailed(msg) => write!(f, "Authorization failed: {msg}"),
            PsmError::InvalidInput(msg) => write!(f, "Invalid input: {msg}"),
            PsmError::StorageError(msg) => write!(f, "Storage error: {msg}"),
            PsmError::NetworkError(msg) => write!(f, "Network error: {msg}"),
            PsmError::SigningError(msg) => write!(f, "Signing error: {msg}"),
            PsmError::ConfigurationError(msg) => write!(f, "Configuration error: {msg}"),
            PsmError::ProposalNotFound {
                account_id,
                commitment,
            } => {
                write!(
                    f,
                    "Proposal not found for account '{account_id}' with commitment '{commitment}'"
                )
            }
            PsmError::ProposalAlreadySigned { signer_id } => {
                write!(f, "Proposal already signed by '{signer_id}'")
            }
            PsmError::InvalidProposalSignature(msg) => {
                write!(f, "Invalid proposal signature: {msg}")
            }
            PsmError::InsufficientSignatures { required, got } => {
                write!(f, "Insufficient signatures: required {required}, got {got}")
            }
        }
    }
}

impl std::error::Error for PsmError {}

impl From<String> for PsmError {
    fn from(s: String) -> Self {
        PsmError::InvalidInput(s)
    }
}

impl From<&str> for PsmError {
    fn from(s: &str) -> Self {
        PsmError::InvalidInput(s.to_string())
    }
}

impl From<MidenFalconRpoError> for PsmError {
    fn from(err: MidenFalconRpoError) -> Self {
        PsmError::SigningError(err.to_string())
    }
}

impl From<miden_keystore::KeyStoreError> for PsmError {
    fn from(err: miden_keystore::KeyStoreError) -> Self {
        PsmError::SigningError(err.to_string())
    }
}

#[derive(Serialize)]
struct ErrorResponse {
    success: bool,
    error: String,
}

impl IntoResponse for PsmError {
    fn into_response(self) -> Response {
        let status = self.http_status();
        let body = Json(ErrorResponse {
            success: false,
            error: self.to_string(),
        });
        (status, body).into_response()
    }
}

impl From<PsmError> for tonic::Status {
    fn from(err: PsmError) -> Self {
        tonic::Status::new(err.grpc_status(), err.to_string())
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
