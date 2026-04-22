use crate::error::GuardianError;
use axum::{extract::FromRequestParts, http::request::Parts};
use guardian_shared::auth_request_payload::AuthRequestPayload;

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
        request_payload: AuthRequestPayload,
    },
}

impl Credentials {
    pub fn signature(pubkey: String, signature: String, timestamp: i64) -> Self {
        Self::Signature {
            pubkey,
            signature,
            timestamp,
            request_payload: AuthRequestPayload::empty(),
        }
    }

    pub fn as_signature(&self) -> Option<(&str, &str, i64)> {
        match self {
            Self::Signature {
                pubkey,
                signature,
                timestamp,
                ..
            } => Some((pubkey, signature, *timestamp)),
        }
    }

    pub fn timestamp(&self) -> i64 {
        match self {
            Self::Signature { timestamp, .. } => *timestamp,
        }
    }

    pub fn with_request_payload(mut self, request_payload: AuthRequestPayload) -> Self {
        match &mut self {
            Self::Signature {
                request_payload: payload,
                ..
            } => {
                *payload = request_payload;
            }
        }
        self
    }

    pub fn request_payload(&self) -> &AuthRequestPayload {
        match self {
            Self::Signature {
                request_payload, ..
            } => request_payload,
        }
    }
}

/// Typed HTTP auth extractor to remove header parsing duplication
pub struct AuthHeader(pub Credentials);

impl<S> FromRequestParts<S> for AuthHeader
where
    S: Send + Sync,
{
    type Rejection = GuardianError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let creds = parts
            .headers
            .extract_credentials()
            .map_err(GuardianError::AuthenticationFailed)?;
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

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;

    #[test]
    fn test_credentials_signature_constructor() {
        let creds =
            Credentials::signature("pubkey123".to_string(), "sig456".to_string(), 1700000000000);

        match creds {
            Credentials::Signature {
                pubkey,
                signature,
                timestamp,
                request_payload,
            } => {
                assert_eq!(pubkey, "pubkey123");
                assert_eq!(signature, "sig456");
                assert_eq!(timestamp, 1700000000000);
                assert_eq!(request_payload, AuthRequestPayload::empty());
            }
        }
    }

    #[test]
    fn test_credentials_as_signature() {
        let creds =
            Credentials::signature("pubkey123".to_string(), "sig456".to_string(), 1700000000000);

        let (pubkey, sig, ts) = creds.as_signature().expect("Should return Some");
        assert_eq!(pubkey, "pubkey123");
        assert_eq!(sig, "sig456");
        assert_eq!(ts, 1700000000000);
    }

    #[test]
    fn test_credentials_timestamp() {
        let creds = Credentials::signature("pubkey".to_string(), "sig".to_string(), 1700000000000);

        assert_eq!(creds.timestamp(), 1700000000000);
    }

    #[test]
    fn test_extract_credentials_from_headers_success() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-pubkey", "0xpubkey".parse().unwrap());
        headers.insert("x-signature", "0xsignature".parse().unwrap());
        headers.insert("x-timestamp", "1700000000000".parse().unwrap());

        let creds = headers.extract_credentials().expect("Should succeed");
        let (pubkey, sig, ts) = creds.as_signature().unwrap();
        assert_eq!(pubkey, "0xpubkey");
        assert_eq!(sig, "0xsignature");
        assert_eq!(ts, 1700000000000);
    }

    #[test]
    fn test_extract_credentials_missing_pubkey() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-signature", "0xsignature".parse().unwrap());
        headers.insert("x-timestamp", "1700000000000".parse().unwrap());

        let result = headers.extract_credentials();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Missing x-pubkey"));
    }

    #[test]
    fn test_extract_credentials_missing_signature() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-pubkey", "0xpubkey".parse().unwrap());
        headers.insert("x-timestamp", "1700000000000".parse().unwrap());

        let result = headers.extract_credentials();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Missing x-signature"));
    }

    #[test]
    fn test_extract_credentials_missing_timestamp() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-pubkey", "0xpubkey".parse().unwrap());
        headers.insert("x-signature", "0xsignature".parse().unwrap());

        let result = headers.extract_credentials();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Missing x-timestamp"));
    }

    #[test]
    fn test_extract_credentials_invalid_timestamp() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-pubkey", "0xpubkey".parse().unwrap());
        headers.insert("x-signature", "0xsignature".parse().unwrap());
        headers.insert("x-timestamp", "not-a-number".parse().unwrap());

        let result = headers.extract_credentials();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid x-timestamp value"));
    }

    #[test]
    fn test_extract_credentials_from_grpc_metadata_success() {
        let mut metadata = tonic::metadata::MetadataMap::new();
        metadata.insert("x-pubkey", "0xpubkey".parse().unwrap());
        metadata.insert("x-signature", "0xsignature".parse().unwrap());
        metadata.insert("x-timestamp", "1700000000000".parse().unwrap());

        let creds = metadata.extract_credentials().expect("Should succeed");
        let (pubkey, sig, ts) = creds.as_signature().unwrap();
        assert_eq!(pubkey, "0xpubkey");
        assert_eq!(sig, "0xsignature");
        assert_eq!(ts, 1700000000000);
    }

    #[test]
    fn test_extract_credentials_from_grpc_missing_pubkey() {
        let mut metadata = tonic::metadata::MetadataMap::new();
        metadata.insert("x-signature", "0xsignature".parse().unwrap());
        metadata.insert("x-timestamp", "1700000000000".parse().unwrap());

        let result = metadata.extract_credentials();
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_credentials_from_grpc_invalid_timestamp() {
        let mut metadata = tonic::metadata::MetadataMap::new();
        metadata.insert("x-pubkey", "0xpubkey".parse().unwrap());
        metadata.insert("x-signature", "0xsignature".parse().unwrap());
        metadata.insert("x-timestamp", "not-a-number".parse().unwrap());

        let result = metadata.extract_credentials();
        assert!(result.is_err());
    }
}
