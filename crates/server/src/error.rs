use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
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
    EvmSupportDisabled,
    UnsupportedForNetwork {
        network: String,
        operation: String,
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

/// Signing-specific error type for Miden ECDSA operations
#[derive(Debug)]
pub enum MidenEcdsaError {
    StorageError(String),
    DecodingError(String),
}

/// Result type alias for Miden ECDSA signing operations
pub type MidenEcdsaResult<T> = std::result::Result<T, MidenEcdsaError>;

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
            GuardianError::AuthorizationFailed(_) => StatusCode::FORBIDDEN,
            GuardianError::InvalidInput(_) => StatusCode::BAD_REQUEST,
            GuardianError::InvalidAccountId(_) => StatusCode::BAD_REQUEST,
            GuardianError::InvalidDelta(_) => StatusCode::BAD_REQUEST,
            GuardianError::InvalidCommitment(_) => StatusCode::BAD_REQUEST,
            GuardianError::CommitmentMismatch { .. } => StatusCode::BAD_REQUEST,
            GuardianError::InvalidProposalSignature(_) => StatusCode::BAD_REQUEST,
            GuardianError::EvmSupportDisabled => StatusCode::BAD_REQUEST,
            GuardianError::UnsupportedForNetwork { .. } => StatusCode::BAD_REQUEST,
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
            GuardianError::AuthorizationFailed(_) => tonic::Code::PermissionDenied,
            GuardianError::InvalidInput(_) => tonic::Code::InvalidArgument,
            GuardianError::InvalidAccountId(_) => tonic::Code::InvalidArgument,
            GuardianError::InvalidDelta(_) => tonic::Code::InvalidArgument,
            GuardianError::InvalidCommitment(_) => tonic::Code::InvalidArgument,
            GuardianError::CommitmentMismatch { .. } => tonic::Code::InvalidArgument,
            GuardianError::InvalidProposalSignature(_) => tonic::Code::InvalidArgument,
            GuardianError::EvmSupportDisabled => tonic::Code::FailedPrecondition,
            GuardianError::UnsupportedForNetwork { .. } => tonic::Code::FailedPrecondition,
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
            GuardianError::AuthorizationFailed(_) => "authorization_failed",
            GuardianError::InvalidInput(_) => "invalid_input",
            GuardianError::StorageError(_) => "storage_error",
            GuardianError::NetworkError(_) => "network_error",
            GuardianError::SigningError(_) => "signing_error",
            GuardianError::ConfigurationError(_) => "configuration_error",
            GuardianError::ProposalNotFound { .. } => "proposal_not_found",
            GuardianError::ProposalAlreadySigned { .. } => "proposal_already_signed",
            GuardianError::InvalidProposalSignature(_) => "invalid_proposal_signature",
            GuardianError::EvmSupportDisabled => "evm_support_disabled",
            GuardianError::UnsupportedForNetwork { .. } => "unsupported_for_network",
            GuardianError::InvalidNetworkConfig(_) => "invalid_network_config",
            GuardianError::RpcUnavailable(_) => "rpc_unavailable",
            GuardianError::RpcValidationFailed(_) => "rpc_validation_failed",
            GuardianError::SignerNotAuthorized(_) => "signer_not_authorized",
            GuardianError::InvalidEvmProposal(_) => "invalid_evm_proposal",
            GuardianError::InsufficientSignatures { .. } => "insufficient_signatures",
            GuardianError::RateLimitExceeded { .. } => "rate_limit_exceeded",
        }
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
            GuardianError::EvmSupportDisabled => {
                write!(f, "EVM support is disabled")
            }
            GuardianError::UnsupportedForNetwork { network, operation } => {
                write!(
                    f,
                    "Operation '{operation}' is unsupported for {network} accounts"
                )
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

#[derive(Serialize)]
struct ErrorResponse {
    success: bool,
    code: &'static str,
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_secs: Option<u32>,
}

impl IntoResponse for GuardianError {
    fn into_response(self) -> Response {
        let status = self.http_status();
        let retry_after_secs = match &self {
            GuardianError::RateLimitExceeded {
                retry_after_secs, ..
            } => Some(*retry_after_secs),
            _ => None,
        };
        let body = Json(ErrorResponse {
            success: false,
            code: self.code(),
            error: self.to_string(),
            retry_after_secs,
        });
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

impl From<GuardianError> for tonic::Status {
    fn from(err: GuardianError) -> Self {
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

impl From<MidenEcdsaError> for GuardianError {
    fn from(err: MidenEcdsaError) -> Self {
        GuardianError::SigningError(err.to_string())
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
    fn from_miden_ecdsa_error() {
        let err = MidenEcdsaError::DecodingError("decode fail".into());
        let guardian: GuardianError = err.into();
        assert!(matches!(guardian, GuardianError::SigningError(_)));
        assert!(guardian.to_string().contains("decode fail"));
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
        let err = GuardianError::AccountNotFound("x".into());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn into_tonic_status() {
        let err = GuardianError::AuthenticationFailed("bad creds".into());
        let status: tonic::Status = err.into();
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
        assert!(status.message().contains("bad creds"));
    }
}
