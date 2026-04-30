use axum::http::{HeaderName, HeaderValue, Method, header};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

const ALLOWED_ORIGINS_ENV: &str = "GUARDIAN_CORS_ALLOWED_ORIGINS";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorsConfig {
    allowed_origins: Vec<HeaderValue>,
}

impl CorsConfig {
    pub fn from_env() -> Result<Self, String> {
        let allowed_origins = match std::env::var(ALLOWED_ORIGINS_ENV) {
            Ok(value) => parse_allowed_origins(&value)?,
            Err(_) => Vec::new(),
        };

        Ok(Self { allowed_origins })
    }

    pub fn new(allowed_origins: Vec<HeaderValue>) -> Self {
        Self { allowed_origins }
    }

    pub fn layer(&self) -> CorsLayer {
        if self.allowed_origins.is_empty() {
            return CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any);
        }

        CorsLayer::new()
            .allow_origin(AllowOrigin::list(self.allowed_origins.clone()))
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                header::CONTENT_TYPE,
                header::AUTHORIZATION,
                HeaderName::from_static("x-pubkey"),
                HeaderName::from_static("x-signature"),
                HeaderName::from_static("x-timestamp"),
            ])
            .allow_credentials(true)
    }
}

fn parse_allowed_origins(value: &str) -> Result<Vec<HeaderValue>, String> {
    let mut origins = Vec::new();
    for origin in value
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
    {
        if origin == "*" {
            return Err(format!(
                "{ALLOWED_ORIGINS_ENV} must use explicit origins for credentialed CORS"
            ));
        }
        let header_value = HeaderValue::from_str(origin)
            .map_err(|_| format!("{ALLOWED_ORIGINS_ENV} contains an invalid origin: {origin}"))?;
        origins.push(header_value);
    }
    Ok(origins)
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::parse_allowed_origins;

    #[test]
    fn parses_explicit_origin_allowlist() {
        let origins = parse_allowed_origins(
            "https://accounts.openzeppelin.com, https://admin.openzeppelin.com",
        )
        .expect("origins");

        assert_eq!(origins.len(), 2);
        assert_eq!(
            origins[0],
            HeaderValue::from_static("https://accounts.openzeppelin.com")
        );
    }

    #[test]
    fn rejects_wildcard_origins() {
        let error = parse_allowed_origins("*").expect_err("wildcard should fail");

        assert!(error.contains("explicit origins"));
    }
}
