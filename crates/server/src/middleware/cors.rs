use axum::http::{HeaderName, HeaderValue, Method, header};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

const ALLOWED_ORIGINS_ENV: &str = "GUARDIAN_CORS_ALLOWED_ORIGINS";
const ALLOW_CREDENTIALS_ENV: &str = "GUARDIAN_CORS_ALLOW_CREDENTIALS";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorsConfig {
    allowed_origins: Vec<HeaderValue>,
    allow_credentials: bool,
}

impl CorsConfig {
    pub fn from_env() -> Result<Self, String> {
        let allowed_origins = match std::env::var(ALLOWED_ORIGINS_ENV) {
            Ok(value) => parse_allowed_origins(&value)?,
            Err(_) => Vec::new(),
        };
        let allow_credentials = match std::env::var(ALLOW_CREDENTIALS_ENV) {
            Ok(value) => parse_bool(ALLOW_CREDENTIALS_ENV, &value)?,
            Err(_) => false,
        };

        Self::new(allowed_origins, allow_credentials)
    }

    pub fn new(allowed_origins: Vec<HeaderValue>, allow_credentials: bool) -> Result<Self, String> {
        if allow_credentials && allowed_origins.is_empty() {
            return Err(format!(
                "{ALLOW_CREDENTIALS_ENV}=true requires {ALLOWED_ORIGINS_ENV}"
            ));
        }

        Ok(Self {
            allowed_origins,
            allow_credentials,
        })
    }

    pub fn layer(&self) -> CorsLayer {
        let layer = CorsLayer::new();
        let layer = if self.allowed_origins.is_empty() {
            layer.allow_origin(Any)
        } else {
            layer.allow_origin(AllowOrigin::list(self.allowed_origins.clone()))
        };

        if self.allow_credentials {
            layer
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
        } else {
            layer.allow_methods(Any).allow_headers(Any)
        }
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
                "{ALLOWED_ORIGINS_ENV} must use explicit origins; wildcard is not supported"
            ));
        }
        let header_value = HeaderValue::from_str(origin)
            .map_err(|_| format!("{ALLOWED_ORIGINS_ENV} contains an invalid origin: {origin}"))?;
        origins.push(header_value);
    }
    Ok(origins)
}

fn parse_bool(name: &str, value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(format!("{name} must be true or false")),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::{CorsConfig, parse_allowed_origins};

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

    #[test]
    fn credentialed_cors_requires_origins() {
        let error = CorsConfig::new(Vec::new(), true).expect_err("missing origins should fail");

        assert!(error.contains("GUARDIAN_CORS_ALLOWED_ORIGINS"));
    }
}
