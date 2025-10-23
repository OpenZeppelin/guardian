use crate::error::PsmError;
use axum::{extract::FromRequestParts, http::request::Parts};

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
    /// Public key signature-based authentication
    /// Used for cryptographic signature verification (e.g., Falcon, ECDSA, etc.)
    Signature { pubkey: String, signature: String },
}

impl Credentials {
    pub fn signature(pubkey: String, signature: String) -> Self {
        Self::Signature { pubkey, signature }
    }

    pub fn as_signature(&self) -> Option<(&str, &str)> {
        match self {
            Self::Signature { pubkey, signature } => Some((pubkey, signature)),
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

        Ok(Credentials::signature(pubkey, signature))
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

        Ok(Credentials::signature(pubkey, signature))
    }
}
