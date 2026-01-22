use crate::error::PsmError;
use axum::{extract::FromRequestParts, http::request::Parts};

/// Maximum allowed clock skew in milliseconds between client and server timestamps
pub const MAX_TIMESTAMP_SKEW_MS: i64 = 300_000; // 5 minutes in milliseconds

/// Trait for extracting authentication credentials from request metadata
/// Implemented by HTTP headers and gRPC metadata
pub trait ExtractCredentials {
    type Error;

    /// Extract credentials from the metadata source
    fn extract_credentials(&self) -> Result<Credentials, Self::Error>;
}

/// Authentication credentials enum - extensible for different auth methods
#[derive(Debug, Clone)]
pub enum Credentials {
    /// Public key signature-based authentication with timestamp
    /// Used for cryptographic signature verification (e.g., Falcon, ECDSA, etc.)
    Signature {
        pubkey: String,
        signature: String,
        timestamp: i64,
    },
}

impl Credentials {
    pub fn signature(pubkey: String, signature: String, timestamp: i64) -> Self {
        Self::Signature {
            pubkey,
            signature,
            timestamp,
        }
    }

    pub fn as_signature(&self) -> Option<(&str, &str, i64)> {
        match self {
            Self::Signature {
                pubkey,
                signature,
                timestamp,
            } => Some((pubkey, signature, *timestamp)),
        }
    }

    pub fn timestamp(&self) -> i64 {
        match self {
            Self::Signature { timestamp, .. } => *timestamp,
        }
    }
}

/// Typed HTTP auth extractor to remove header parsing duplication
pub struct AuthHeader(pub Credentials);

impl<S> FromRequestParts<S> for AuthHeader
where
    S: Send + Sync,
{
    type Rejection = PsmError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let creds = parts
            .headers
            .extract_credentials()
            .map_err(PsmError::AuthenticationFailed)?;
        Ok(AuthHeader(creds))
    }
}

impl ExtractCredentials for axum::http::HeaderMap {
    type Error = String;

    fn extract_credentials(&self) -> Result<Credentials, Self::Error> {
        let pubkey = self
            .get("x-pubkey")
            .ok_or_else(|| "Missing x-pubkey header".to_string())?
            .to_str()
            .map_err(|_| "Invalid x-pubkey header".to_string())?
            .to_string();

        let signature = self
            .get("x-signature")
            .ok_or_else(|| "Missing x-signature header".to_string())?
            .to_str()
            .map_err(|_| "Invalid x-signature header".to_string())?
            .to_string();

        let timestamp = self
            .get("x-timestamp")
            .ok_or_else(|| "Missing x-timestamp header".to_string())?
            .to_str()
            .map_err(|_| "Invalid x-timestamp header".to_string())?
            .parse::<i64>()
            .map_err(|_| "Invalid x-timestamp value: must be Unix timestamp".to_string())?;

        Ok(Credentials::signature(pubkey, signature, timestamp))
    }
}

impl ExtractCredentials for tonic::metadata::MetadataMap {
    type Error = tonic::Status;

    fn extract_credentials(&self) -> Result<Credentials, Self::Error> {
        let pubkey = self
            .get("x-pubkey")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| tonic::Status::invalid_argument("Missing or invalid x-pubkey metadata"))?
            .to_string();

        let signature = self
            .get("x-signature")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                tonic::Status::invalid_argument("Missing or invalid x-signature metadata")
            })?
            .to_string();

        let timestamp = self
            .get("x-timestamp")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                tonic::Status::invalid_argument("Missing or invalid x-timestamp metadata")
            })?
            .parse::<i64>()
            .map_err(|_| {
                tonic::Status::invalid_argument("Invalid x-timestamp value: must be Unix timestamp")
            })?;

        Ok(Credentials::signature(pubkey, signature, timestamp))
    }
}
