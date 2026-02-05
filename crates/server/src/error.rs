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

/// Signing-specific error type for Miden ECDSA operations
#[derive(Debug)]
pub enum MidenEcdsaError {
    StorageError(String),
    DecodingError(String),
}

/// Result type alias for Miden ECDSA signing operations
pub type MidenEcdsaResult<T> = std::result::Result<T, MidenEcdsaError>;

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

impl fmt::Display for MidenEcdsaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MidenEcdsaError::StorageError(msg) => write!(f, "ECDSA storage error: {msg}"),
            MidenEcdsaError::DecodingError(msg) => write!(f, "ECDSA decoding error: {msg}"),
        }
    }
}

impl std::error::Error for MidenEcdsaError {}

impl From<MidenEcdsaError> for PsmError {
    fn from(err: MidenEcdsaError) -> Self {
        PsmError::SigningError(err.to_string())
    }
}

impl From<miden_keystore::KeyStoreError> for MidenEcdsaError {
    fn from(err: miden_keystore::KeyStoreError) -> Self {
        match err {
            miden_keystore::KeyStoreError::StorageError(msg) => MidenEcdsaError::StorageError(msg),
            miden_keystore::KeyStoreError::DecodingError(msg) => {
                MidenEcdsaError::DecodingError(msg)
            }
            miden_keystore::KeyStoreError::KeyNotFound(msg) => MidenEcdsaError::StorageError(msg),
        }
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;

    // --- PsmError::http_status ---

    #[test]
    fn http_status_not_found_variants() {
        assert_eq!(
            PsmError::AccountNotFound("x".into()).http_status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            PsmError::DeltaNotFound {
                account_id: "x".into(),
                nonce: 1
            }
            .http_status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            PsmError::StateNotFound("x".into()).http_status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            PsmError::ProposalNotFound {
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
            PsmError::AccountAlreadyExists("x".into()).http_status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            PsmError::ConflictPendingDelta.http_status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            PsmError::ConflictPendingProposal.http_status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            PsmError::ProposalAlreadySigned {
                signer_id: "s".into()
            }
            .http_status(),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn http_status_auth_variants() {
        assert_eq!(
            PsmError::AuthenticationFailed("x".into()).http_status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            PsmError::AuthorizationFailed("x".into()).http_status(),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn http_status_bad_request_variants() {
        assert_eq!(
            PsmError::InvalidInput("x".into()).http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            PsmError::InvalidAccountId("x".into()).http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            PsmError::InvalidDelta("x".into()).http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            PsmError::InvalidCommitment("x".into()).http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            PsmError::CommitmentMismatch {
                expected: "a".into(),
                actual: "b".into()
            }
            .http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            PsmError::InvalidProposalSignature("x".into()).http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            PsmError::InsufficientSignatures {
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
            PsmError::NetworkError("x".into()).http_status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            PsmError::SigningError("x".into()).http_status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            PsmError::StorageError("x".into()).http_status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            PsmError::ConfigurationError("x".into()).http_status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    // --- PsmError::grpc_status ---

    #[test]
    fn grpc_status_not_found() {
        assert_eq!(
            PsmError::AccountNotFound("x".into()).grpc_status(),
            tonic::Code::NotFound
        );
        assert_eq!(
            PsmError::StateNotFound("x".into()).grpc_status(),
            tonic::Code::NotFound
        );
    }

    #[test]
    fn grpc_status_already_exists() {
        assert_eq!(
            PsmError::AccountAlreadyExists("x".into()).grpc_status(),
            tonic::Code::AlreadyExists
        );
        assert_eq!(
            PsmError::ProposalAlreadySigned {
                signer_id: "s".into()
            }
            .grpc_status(),
            tonic::Code::AlreadyExists
        );
    }

    #[test]
    fn grpc_status_failed_precondition() {
        assert_eq!(
            PsmError::ConflictPendingDelta.grpc_status(),
            tonic::Code::FailedPrecondition
        );
        assert_eq!(
            PsmError::ConflictPendingProposal.grpc_status(),
            tonic::Code::FailedPrecondition
        );
        assert_eq!(
            PsmError::InsufficientSignatures {
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
            PsmError::AuthenticationFailed("x".into()).grpc_status(),
            tonic::Code::Unauthenticated
        );
        assert_eq!(
            PsmError::AuthorizationFailed("x".into()).grpc_status(),
            tonic::Code::PermissionDenied
        );
    }

    #[test]
    fn grpc_status_invalid_argument() {
        assert_eq!(
            PsmError::InvalidInput("x".into()).grpc_status(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            PsmError::InvalidAccountId("x".into()).grpc_status(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            PsmError::InvalidDelta("x".into()).grpc_status(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            PsmError::InvalidCommitment("x".into()).grpc_status(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            PsmError::CommitmentMismatch {
                expected: "a".into(),
                actual: "b".into()
            }
            .grpc_status(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            PsmError::InvalidProposalSignature("x".into()).grpc_status(),
            tonic::Code::InvalidArgument
        );
    }

    #[test]
    fn grpc_status_internal() {
        assert_eq!(
            PsmError::SigningError("x".into()).grpc_status(),
            tonic::Code::Internal
        );
        assert_eq!(
            PsmError::StorageError("x".into()).grpc_status(),
            tonic::Code::Internal
        );
        assert_eq!(
            PsmError::ConfigurationError("x".into()).grpc_status(),
            tonic::Code::Internal
        );
    }

    // --- Display ---

    #[test]
    fn display_account_not_found() {
        let err = PsmError::AccountNotFound("abc".into());
        assert_eq!(err.to_string(), "Account 'abc' not found");
    }

    #[test]
    fn display_account_already_exists() {
        let err = PsmError::AccountAlreadyExists("abc".into());
        assert_eq!(err.to_string(), "Account 'abc' already exists");
    }

    #[test]
    fn display_delta_not_found() {
        let err = PsmError::DeltaNotFound {
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
        let err = PsmError::CommitmentMismatch {
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
            PsmError::ConflictPendingDelta
                .to_string()
                .contains("non-canonical delta pending")
        );
    }

    #[test]
    fn display_conflict_pending_proposal() {
        assert!(
            PsmError::ConflictPendingProposal
                .to_string()
                .contains("pending proposals")
        );
    }

    #[test]
    fn display_proposal_not_found() {
        let err = PsmError::ProposalNotFound {
            account_id: "acc".into(),
            commitment: "c".into(),
        };
        assert!(err.to_string().contains("acc"));
        assert!(err.to_string().contains("c"));
    }

    #[test]
    fn display_proposal_already_signed() {
        let err = PsmError::ProposalAlreadySigned {
            signer_id: "signer".into(),
        };
        assert!(err.to_string().contains("signer"));
    }

    #[test]
    fn display_insufficient_signatures() {
        let err = PsmError::InsufficientSignatures {
            required: 3,
            got: 1,
        };
        assert!(err.to_string().contains("3"));
        assert!(err.to_string().contains("1"));
    }

    // --- From conversions ---

    #[test]
    fn from_string_creates_invalid_input() {
        let err: PsmError = "some error".to_string().into();
        assert_eq!(err, PsmError::InvalidInput("some error".into()));
    }

    #[test]
    fn from_str_creates_invalid_input() {
        let err: PsmError = "some error".into();
        assert_eq!(err, PsmError::InvalidInput("some error".into()));
    }

    #[test]
    fn from_miden_falcon_rpo_error() {
        let err = MidenFalconRpoError::StorageError("storage fail".into());
        let psm: PsmError = err.into();
        assert!(matches!(psm, PsmError::SigningError(_)));
        assert!(psm.to_string().contains("storage fail"));
    }

    #[test]
    fn from_miden_ecdsa_error() {
        let err = MidenEcdsaError::DecodingError("decode fail".into());
        let psm: PsmError = err.into();
        assert!(matches!(psm, PsmError::SigningError(_)));
        assert!(psm.to_string().contains("decode fail"));
    }

    #[test]
    fn from_keystore_error_to_psm() {
        let err = miden_keystore::KeyStoreError::KeyNotFound("key123".into());
        let psm: PsmError = err.into();
        assert!(matches!(psm, PsmError::SigningError(_)));
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

    // --- MidenEcdsaError Display ---

    #[test]
    fn ecdsa_error_display() {
        assert!(
            MidenEcdsaError::StorageError("x".into())
                .to_string()
                .contains("ECDSA storage error")
        );
        assert!(
            MidenEcdsaError::DecodingError("y".into())
                .to_string()
                .contains("ECDSA decoding error")
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

    // --- KeyStoreError -> MidenEcdsaError ---

    #[test]
    fn keystore_error_to_ecdsa_storage() {
        let err = miden_keystore::KeyStoreError::StorageError("s".into());
        let ecdsa: MidenEcdsaError = err.into();
        assert!(matches!(ecdsa, MidenEcdsaError::StorageError(_)));
    }

    #[test]
    fn keystore_error_to_ecdsa_decoding() {
        let err = miden_keystore::KeyStoreError::DecodingError("d".into());
        let ecdsa: MidenEcdsaError = err.into();
        assert!(matches!(ecdsa, MidenEcdsaError::DecodingError(_)));
    }

    #[test]
    fn keystore_error_to_ecdsa_key_not_found() {
        let err = miden_keystore::KeyStoreError::KeyNotFound("k".into());
        let ecdsa: MidenEcdsaError = err.into();
        assert!(matches!(ecdsa, MidenEcdsaError::StorageError(_)));
    }

    // --- IntoResponse / tonic::Status ---

    #[test]
    fn into_response_returns_correct_status() {
        let err = PsmError::AccountNotFound("x".into());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn into_tonic_status() {
        let err = PsmError::AuthenticationFailed("bad creds".into());
        let status: tonic::Status = err.into();
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
        assert!(status.message().contains("bad creds"));
    }
}
