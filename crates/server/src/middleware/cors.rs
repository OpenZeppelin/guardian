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
        // `Retry-After` is not a CORS-safelisted response header; without an
        // explicit expose, cross-origin browser clients cannot read the
        // rate-limit backoff hint.
        if self.allowed_origins.is_empty() {
            return CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any)
                .expose_headers([header::RETRY_AFTER]);
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
            .expose_headers([header::RETRY_AFTER])
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

    #[tokio::test]
    async fn both_configurations_expose_retry_after_cross_origin() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::{ServiceBuilder, ServiceExt};

        let configs = [
            super::CorsConfig::new(Vec::new()),
            super::CorsConfig::new(vec![HeaderValue::from_static("https://app.example.com")]),
        ];

        for config in configs {
            let service = ServiceBuilder::new()
                .layer(config.layer())
                .service(tower::service_fn(|_request: Request<Body>| async {
                    Ok::<_, std::convert::Infallible>(
                        axum::http::Response::builder()
                            .header("Retry-After", "60")
                            .body(Body::empty())
                            .unwrap(),
                    )
                }));

            let response = service
                .oneshot(
                    Request::builder()
                        .uri("/pubkey")
                        .header("Origin", "https://app.example.com")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            let exposed = response
                .headers()
                .get("access-control-expose-headers")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            assert!(
                exposed.contains("retry-after"),
                "browser clients must be able to read Retry-After, got {exposed:?}"
            );
        }
    }
}
