//! Rate limiting middleware for HTTP endpoints
//!
//! Applies IP-based rate limiting with optional account/signer enhancement.
//! Uses two windows: burst (per second) and sustained (per minute).

use axum::{
    Json,
    body::Body,
    extract::ConnectInfo,
    http::{Request, Response, StatusCode},
    response::IntoResponse,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    env,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, RwLock},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tower::{Layer, Service};

/// Default burst limit: requests per second
const DEFAULT_BURST_PER_SEC: u32 = 10;
/// Default sustained limit: requests per minute
const DEFAULT_PER_MIN: u32 = 60;
/// Cleanup interval for stale entries
const CLEANUP_INTERVAL_SECS: u64 = 60;

/// Rate limit configuration loaded from environment
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum requests per second (burst)
    pub burst_per_sec: u32,
    /// Maximum requests per minute (sustained)
    pub per_min: u32,
}

impl RateLimitConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Self {
        let burst_per_sec = env::var("PSM_RATE_BURST_PER_SEC")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_BURST_PER_SEC);

        let per_min = env::var("PSM_RATE_PER_MIN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_PER_MIN);

        Self {
            burst_per_sec,
            per_min,
        }
    }

    /// Create a new config with custom values
    pub fn new(burst_per_sec: u32, per_min: u32) -> Self {
        Self {
            burst_per_sec,
            per_min,
        }
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            burst_per_sec: DEFAULT_BURST_PER_SEC,
            per_min: DEFAULT_PER_MIN,
        }
    }
}

/// Tracks request counts for a single key
#[derive(Debug, Clone)]
struct RateLimitEntry {
    /// Count of requests in current second
    burst_count: u32,
    /// Start of current second window
    burst_window_start: Instant,
    /// Count of requests in current minute
    sustained_count: u32,
    /// Start of current minute window
    sustained_window_start: Instant,
}

impl RateLimitEntry {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            burst_count: 0,
            burst_window_start: now,
            sustained_count: 0,
            sustained_window_start: now,
        }
    }
}

/// Thread-safe rate limit store
#[derive(Debug, Clone)]
pub struct RateLimitStore {
    entries: Arc<RwLock<HashMap<String, RateLimitEntry>>>,
    config: RateLimitConfig,
    last_cleanup: Arc<RwLock<Instant>>,
}

impl RateLimitStore {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            config,
            last_cleanup: Arc::new(RwLock::new(Instant::now())),
        }
    }

    /// Check if a request should be rate limited for burst window
    /// Returns Ok(()) if allowed, Err(RateLimitType::Burst) if limited
    pub fn check_burst(&self, key: &str) -> Result<(), RateLimitType> {
        self.maybe_cleanup();

        let now = Instant::now();
        let mut entries = self.entries.write().unwrap();
        let entry = entries
            .entry(key.to_string())
            .or_insert_with(RateLimitEntry::new);

        // Check and reset burst window (1 second)
        if now.duration_since(entry.burst_window_start) >= Duration::from_secs(1) {
            entry.burst_count = 0;
            entry.burst_window_start = now;
        }

        // Check burst limit first (more restrictive short-term)
        if entry.burst_count >= self.config.burst_per_sec {
            return Err(RateLimitType::Burst);
        }

        // Increment counters
        entry.burst_count += 1;

        Ok(())
    }

    /// Check if a request should be rate limited for sustained window
    /// Returns Ok(()) if allowed, Err(RateLimitType::Sustained) if limited
    pub fn check_sustained(&self, key: &str) -> Result<(), RateLimitType> {
        self.maybe_cleanup();

        let now = Instant::now();
        let mut entries = self.entries.write().unwrap();
        let entry = entries
            .entry(key.to_string())
            .or_insert_with(RateLimitEntry::new);

        // Check and reset sustained window (1 minute)
        if now.duration_since(entry.sustained_window_start) >= Duration::from_secs(60) {
            entry.sustained_count = 0;
            entry.sustained_window_start = now;
        }

        // Check sustained limit
        if entry.sustained_count >= self.config.per_min {
            return Err(RateLimitType::Sustained);
        }

        // Increment counters
        entry.sustained_count += 1;

        Ok(())
    }

    /// Periodically clean up stale entries
    fn maybe_cleanup(&self) {
        let should_cleanup = {
            let last = self.last_cleanup.read().unwrap();
            last.elapsed() >= Duration::from_secs(CLEANUP_INTERVAL_SECS)
        };

        if should_cleanup {
            let now = Instant::now();
            let mut entries = self.entries.write().unwrap();
            let mut last = self.last_cleanup.write().unwrap();

            // Remove entries that haven't been used in over 2 minutes
            let stale_threshold = Duration::from_secs(120);
            entries.retain(|_, entry| {
                now.duration_since(entry.sustained_window_start) < stale_threshold
            });

            *last = now;
        }
    }
}

/// Type of rate limit exceeded
#[derive(Debug, Clone, Copy)]
pub enum RateLimitType {
    /// Burst limit (per second) exceeded
    Burst,
    /// Sustained limit (per minute) exceeded
    Sustained,
}

impl RateLimitType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Burst => "burst",
            Self::Sustained => "sustained",
        }
    }
}

/// Rate limit error response
#[derive(Debug, Serialize)]
pub struct RateLimitResponse {
    pub success: bool,
    pub error: String,
    pub retry_after_secs: u32,
}

/// Tower layer for rate limiting
#[derive(Debug, Clone)]
pub struct RateLimitLayer {
    store: RateLimitStore,
}

impl RateLimitLayer {
    pub fn new(config: RateLimitConfig) -> Self {
        tracing::info!(
            burst_per_sec = config.burst_per_sec,
            per_min = config.per_min,
            "Rate limiter initialized"
        );
        Self {
            store: RateLimitStore::new(config),
        }
    }

    pub fn from_env() -> Self {
        Self::new(RateLimitConfig::from_env())
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            inner,
            store: self.store.clone(),
        }
    }
}

/// Rate limiting service wrapper
#[derive(Debug, Clone)]
pub struct RateLimitService<S> {
    inner: S,
    store: RateLimitStore,
}

impl<S> Service<Request<Body>> for RateLimitService<S>
where
    S: Service<Request<Body>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let store = self.store.clone();
        let mut inner = self.inner.clone();

        Box::pin(async move {
            // Extract client IP
            let client_ip = extract_client_ip(&req);

            // Extract optional account/signer for enhanced keying
            let enhanced_key = extract_enhanced_key(&req);
            let endpoint = req.uri().path().to_string();

            let mut burst_keys = vec![format!("ip:{}|endpoint:{}", client_ip, endpoint)];
            let mut sustained_keys = vec![format!("ip:{}", client_ip)];

            if let Some(extra) = enhanced_key.as_ref() {
                burst_keys.push(format!("ip:{}|endpoint:{}|{}", client_ip, endpoint, extra));
                sustained_keys.push(format!("ip:{}|{}", client_ip, extra));
            }

            let mut limited: Option<(RateLimitType, String)> = None;

            for key in &burst_keys {
                if let Err(limit_type) = store.check_burst(key) {
                    limited = Some((limit_type, key.clone()));
                    break;
                }
            }

            if limited.is_none() {
                for key in &sustained_keys {
                    if let Err(limit_type) = store.check_sustained(key) {
                        limited = Some((limit_type, key.clone()));
                        break;
                    }
                }
            }

            match limited {
                None => inner.call(req).await,
                Some((limit_type, key)) => {
                    let retry_after = match limit_type {
                        RateLimitType::Burst => 1,
                        RateLimitType::Sustained => 60,
                    };

                    // Log the throttled request
                    tracing::warn!(
                        client_ip = %client_ip,
                        rate_limit_key = %key,
                        limit_type = limit_type.as_str(),
                        endpoint = %endpoint,
                        "Request rate limited"
                    );

                    let response = RateLimitResponse {
                        success: false,
                        error: format!(
                            "Rate limit exceeded ({} limit). Retry after {} seconds.",
                            limit_type.as_str(),
                            retry_after
                        ),
                        retry_after_secs: retry_after,
                    };

                    Ok((
                        StatusCode::TOO_MANY_REQUESTS,
                        [("Retry-After", retry_after.to_string())],
                        Json(response),
                    )
                        .into_response())
                }
            }
        })
    }
}

/// Extract client IP from request, preferring forwarded headers.
fn extract_client_ip<B>(req: &Request<B>) -> String {
    // Prefer X-Forwarded-For (set by trusted load balancer).
    if let Some(forwarded) = req.headers().get("x-forwarded-for")
        && let Ok(value) = forwarded.to_str()
        && let Some(first_ip) = value.split(',').next()
    {
        return first_ip.trim().to_string();
    }

    // Try X-Real-IP header (common with reverse proxies).
    if let Some(real_ip) = req.headers().get("x-real-ip")
        && let Ok(value) = real_ip.to_str()
    {
        return value.to_string();
    }

    // Fall back to connection info.
    if let Some(connect_info) = req.extensions().get::<ConnectInfo<SocketAddr>>() {
        return connect_info.0.ip().to_string();
    }

    // Ultimate fallback
    "unknown".to_string()
}

/// Extract account_id or signer pubkey for enhanced rate limit keying
fn extract_enhanced_key<B>(req: &Request<B>) -> Option<String> {
    // Try to get account_id from query params or path
    if let Some(query) = req.uri().query() {
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("account_id=") {
                return Some(format!("account:{}", value));
            }
        }
    }

    // Try to get signer pubkey from headers
    if let Some(pubkey) = req.headers().get("x-pubkey")
        && let Ok(value) = pubkey.to_str()
    {
        // Use first 16 chars of pubkey to keep key short
        let short_key = if value.len() > 16 {
            &value[..16]
        } else {
            value
        };
        return Some(format!("signer:{}", short_key));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit_config_default() {
        let config = RateLimitConfig::default();
        assert_eq!(config.burst_per_sec, DEFAULT_BURST_PER_SEC);
        assert_eq!(config.per_min, DEFAULT_PER_MIN);
    }

    #[test]
    fn test_rate_limit_config_new() {
        let config = RateLimitConfig::new(5, 30);
        assert_eq!(config.burst_per_sec, 5);
        assert_eq!(config.per_min, 30);
    }

    #[test]
    fn test_rate_limit_store_allows_under_limit() {
        let config = RateLimitConfig::new(5, 10);
        let store = RateLimitStore::new(config);

        for _ in 0..5 {
            assert!(store.check_burst("test_key").is_ok());
        }

        for _ in 0..10 {
            assert!(store.check_sustained("test_key_sustained").is_ok());
        }
    }

    #[test]
    fn test_rate_limit_store_burst_limit() {
        let config = RateLimitConfig::new(3, 100);
        let store = RateLimitStore::new(config);

        // First 3 should pass
        for _ in 0..3 {
            assert!(store.check_burst("burst_test").is_ok());
        }

        // 4th should fail with burst limit
        match store.check_burst("burst_test") {
            Err(RateLimitType::Burst) => {}
            other => panic!("Expected burst limit, got {:?}", other),
        }
    }

    #[test]
    fn test_rate_limit_store_sustained_limit() {
        let config = RateLimitConfig::new(100, 5);
        let store = RateLimitStore::new(config);

        // First 5 should pass
        for _ in 0..5 {
            assert!(store.check_sustained("sustained_test").is_ok());
        }

        // 6th should fail with sustained limit
        match store.check_sustained("sustained_test") {
            Err(RateLimitType::Sustained) => {}
            other => panic!("Expected sustained limit, got {:?}", other),
        }
    }

    #[test]
    fn test_rate_limit_store_different_keys() {
        let config = RateLimitConfig::new(2, 10);
        let store = RateLimitStore::new(config);

        // Each key has its own limit
        assert!(store.check_burst("key1").is_ok());
        assert!(store.check_burst("key1").is_ok());
        assert!(store.check_burst("key1").is_err()); // key1 exceeded

        // key2 should still work
        assert!(store.check_burst("key2").is_ok());
        assert!(store.check_burst("key2").is_ok());
    }

    #[test]
    fn test_rate_limit_type_as_str() {
        assert_eq!(RateLimitType::Burst.as_str(), "burst");
        assert_eq!(RateLimitType::Sustained.as_str(), "sustained");
    }
}
