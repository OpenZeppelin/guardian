//! Rate limiting shared by the HTTP and gRPC transports.
//!
//! Applies IP-based rate limiting with optional account/signer enhancement.
//! Uses two windows: burst (per second) and sustained (per minute). One
//! [`RateLimitStore`] backs both transport layers so they draw from a
//! single budget.

use axum::{
    body::Body,
    http::{Request, Response},
    response::IntoResponse,
};
use futures::future::Either;
use std::{
    collections::HashMap,
    env,
    future::{Ready, ready},
    sync::{Arc, RwLock},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tower::{Layer, Service};

/// Default burst limit: requests per second
const DEFAULT_BURST_PER_SEC: u32 = 10;
/// Default sustained limit: requests per minute
const DEFAULT_PER_MIN: u32 = 60;
/// Environment variable for enabling or disabling rate limiting
const ENV_RATE_LIMIT_ENABLED: &str = "GUARDIAN_RATE_LIMIT_ENABLED";
/// Deployment's steady-state replica capacity; configured limits are divided
/// by it so per-process enforcement keeps the steady-state fleet aggregate at
/// or below the fleet-wide limit (issue #242). Drives rate limiting only.
const ENV_MAX_REPLICAS: &str = "GUARDIAN_MAX_REPLICAS";
/// Cleanup interval for stale entries
const CLEANUP_INTERVAL_SECS: u64 = 60;

/// Rate limit configuration loaded from environment
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Whether rate limiting is enabled
    pub enabled: bool,
    /// Maximum requests per second (burst)
    pub burst_per_sec: u32,
    /// Maximum requests per minute (sustained)
    pub per_min: u32,
}

/// Resolved `GUARDIAN_MAX_REPLICAS`: unset means a single replica (`1`); a set
/// value must parse to an integer ≥ 1. A set-but-invalid value is an error
/// rather than a silent `1`: falling back would disable partitioning and let
/// the fleet aggregate reach `max_replicas ×` the global limit — the exact
/// fail-open FR-009 exists to prevent. The prod builder guard turns this error
/// into a startup failure; non-prod callers warn and fall back.
pub(crate) fn max_replicas_from_env() -> Result<u32, String> {
    match env::var(ENV_MAX_REPLICAS) {
        Ok(raw) => match raw.trim().parse::<u32>() {
            Ok(0) => Err(format!(
                "{ENV_MAX_REPLICAS} must be a positive integer, got 0"
            )),
            Ok(value) => Ok(value),
            Err(_) => Err(format!(
                "{ENV_MAX_REPLICAS} must be a positive integer (the autoscaling max \
                 capacity), got {raw:?}"
            )),
        },
        Err(env::VarError::NotPresent) => Ok(1),
        Err(env::VarError::NotUnicode(_)) => {
            Err(format!("{ENV_MAX_REPLICAS} must contain valid UTF-8"))
        }
    }
}

impl RateLimitConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Self {
        let enabled = env_flag(ENV_RATE_LIMIT_ENABLED, true);
        let max_replicas = match max_replicas_from_env() {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "invalid GUARDIAN_MAX_REPLICAS; treating as 1 (no rate-limit \
                     partitioning — the fleet aggregate can exceed the global limit). \
                     The prod stage refuses to start on this instead."
                );
                1
            }
        };
        let burst_per_sec = partition_limit(
            env::var("GUARDIAN_RATE_BURST_PER_SEC")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_BURST_PER_SEC),
            max_replicas,
        );
        let per_min = partition_limit(
            env::var("GUARDIAN_RATE_PER_MIN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_PER_MIN),
            max_replicas,
        );

        if enabled && (burst_per_sec == 0 || per_min == 0) {
            tracing::warn!(
                max_replicas,
                burst_per_sec,
                per_min,
                "rate limit partitions to 0 per replica (global limit is below GUARDIAN_MAX_REPLICAS); \
                 this replica will throttle all traffic. Raise the global rate limit or lower \
                 GUARDIAN_MAX_REPLICAS."
            );
        }

        Self {
            enabled,
            burst_per_sec,
            per_min,
        }
    }

    /// Create a new config with custom values.
    ///
    /// Values are enforced per process, as-is: unlike [`Self::from_env`], no
    /// `GUARDIAN_MAX_REPLICAS` division is applied. Callers wiring explicit
    /// limits into a multi-replica deployment must pass per-replica values.
    pub fn new(burst_per_sec: u32, per_min: u32) -> Self {
        Self {
            enabled: true,
            burst_per_sec,
            per_min,
        }
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            burst_per_sec: DEFAULT_BURST_PER_SEC,
            per_min: DEFAULT_PER_MIN,
        }
    }
}

/// Per-replica share of a global limit: `global / max_replicas` (floor), with
/// `max_replicas` clamped to ≥ 1. The floor — not a round-up or a ≥1 clamp —
/// guarantees the fleet aggregate (`max_replicas × share`) never exceeds the
/// global limit (FR-009). A share of `0` means this replica denies all requests;
/// that only happens when the global limit is below the replica count (an
/// extreme misconfiguration), and it still never exceeds the global limit.
pub(crate) fn partition_limit(global_limit: u32, max_replicas: u32) -> u32 {
    global_limit / max_replicas.max(1)
}

/// Parse a boolean env flag: unset → `default_value`; `0`/`false`/
/// `no`/`off` (case-insensitive) → false; anything else → true.
/// Shared by env-driven configs (rate limiting, metrics).
pub(crate) fn env_flag(key: &str, default_value: bool) -> bool {
    env::var(key)
        .ok()
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(default_value)
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
        if burst_allows(entry_mut(&mut entries, key), self.config.burst_per_sec, now) {
            Ok(())
        } else {
            Err(RateLimitType::Burst)
        }
    }

    /// Check if a request should be rate limited for sustained window
    /// Returns Ok(()) if allowed, Err(RateLimitType::Sustained) if limited
    pub fn check_sustained(&self, key: &str) -> Result<(), RateLimitType> {
        self.maybe_cleanup();

        let now = Instant::now();
        let mut entries = self.entries.write().unwrap();
        if sustained_allows(entry_mut(&mut entries, key), self.config.per_min, now) {
            Ok(())
        } else {
            Err(RateLimitType::Sustained)
        }
    }

    /// Run the full admission check for one request under a single lock
    /// acquisition: burst keys first, then sustained keys, with
    /// `x-pubkey`/`account_id` enhanced keying. `endpoint` scopes the
    /// burst keys; each transport layer derives its own (raw path on
    /// HTTP, normalized proto method on gRPC). When rate limiting is
    /// disabled this returns without deriving keys or touching the
    /// entry map.
    pub fn check_request<B>(
        &self,
        req: &Request<B>,
        endpoint: &str,
    ) -> Result<(), RateLimitRejection> {
        if !self.config.enabled {
            return Ok(());
        }
        self.maybe_cleanup();

        let client_ip = extract_client_ip(req);
        let enhanced_key = extract_enhanced_key(req);

        let burst_key = format!("ip:{client_ip}|endpoint:{endpoint}");
        let sustained_key = format!("ip:{client_ip}");
        let enhanced_burst_key = enhanced_key.as_ref().map(|e| format!("{burst_key}|{e}"));
        let enhanced_sustained_key = enhanced_key
            .as_ref()
            .map(|e| format!("{sustained_key}|{e}"));

        let now = Instant::now();
        let mut entries = self.entries.write().unwrap();

        for key in [Some(&burst_key), enhanced_burst_key.as_ref()]
            .into_iter()
            .flatten()
        {
            if !burst_allows(entry_mut(&mut entries, key), self.config.burst_per_sec, now) {
                return Err(RateLimitRejection {
                    limit_type: RateLimitType::Burst,
                    key: key.clone(),
                    client_ip,
                    endpoint: endpoint.to_string(),
                });
            }
        }

        for key in [Some(&sustained_key), enhanced_sustained_key.as_ref()]
            .into_iter()
            .flatten()
        {
            if !sustained_allows(entry_mut(&mut entries, key), self.config.per_min, now) {
                return Err(RateLimitRejection {
                    limit_type: RateLimitType::Sustained,
                    key: key.clone(),
                    client_ip,
                    endpoint: endpoint.to_string(),
                });
            }
        }

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

/// Fetch-or-create without the owned-key allocation `HashMap::entry`
/// forces on every lookup; the steady state is an existing entry.
fn entry_mut<'a>(
    entries: &'a mut HashMap<String, RateLimitEntry>,
    key: &str,
) -> &'a mut RateLimitEntry {
    if !entries.contains_key(key) {
        entries.insert(key.to_string(), RateLimitEntry::new());
    }
    entries.get_mut(key).expect("entry was just ensured")
}

fn burst_allows(entry: &mut RateLimitEntry, limit: u32, now: Instant) -> bool {
    if now.duration_since(entry.burst_window_start) >= Duration::from_secs(1) {
        entry.burst_count = 0;
        entry.burst_window_start = now;
    }
    if entry.burst_count >= limit {
        return false;
    }
    entry.burst_count += 1;
    true
}

fn sustained_allows(entry: &mut RateLimitEntry, limit: u32, now: Instant) -> bool {
    if now.duration_since(entry.sustained_window_start) >= Duration::from_secs(60) {
        entry.sustained_count = 0;
        entry.sustained_window_start = now;
    }
    if entry.sustained_count >= limit {
        return false;
    }
    entry.sustained_count += 1;
    true
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

    pub fn retry_after_secs(&self) -> u32 {
        match self {
            Self::Burst => 1,
            Self::Sustained => 60,
        }
    }
}

/// One over-budget refusal, carrying everything the rejecting transport
/// needs: the counter labels, the throttle log fields, and the error.
#[derive(Debug)]
pub struct RateLimitRejection {
    limit_type: RateLimitType,
    key: String,
    client_ip: String,
    endpoint: String,
}

impl RateLimitRejection {
    /// Record the rejection (counter + throttle log) and convert it into
    /// the wire error, so no call site can emit the error without the
    /// observability.
    ///
    /// The counter is the always-on signal; the per-rejection line is
    /// `debug` because refusals are expected behavior and their volume
    /// scales with the flood the limiter exists to shed. Raise it with
    /// `RUST_LOG=server::middleware::rate_limit=debug` when a keying
    /// question needs the per-caller detail the counter cannot carry.
    pub fn into_error(self, transport: &'static str) -> crate::error::GuardianError {
        metrics::counter!(
            crate::metrics::names::RATE_LIMIT_REJECTIONS_TOTAL,
            crate::metrics::names::LABEL_LIMIT_TYPE => self.limit_type.as_str(),
            crate::metrics::names::LABEL_TRANSPORT => transport
        )
        .increment(1);

        tracing::debug!(
            client_ip = %self.client_ip,
            rate_limit_key = %self.key,
            limit_type = self.limit_type.as_str(),
            transport,
            endpoint = %self.endpoint,
            "Request rate limited"
        );

        crate::error::GuardianError::RateLimitExceeded {
            retry_after_secs: self.limit_type.retry_after_secs(),
            scope: self.limit_type.as_str().to_string(),
        }
    }
}

/// Tower layer applying a [`RateLimitStore`] to the axum router. The
/// store is passed in, not built here, so HTTP and gRPC share one
/// budget.
#[derive(Debug, Clone)]
pub struct RateLimitLayer {
    store: RateLimitStore,
}

impl RateLimitLayer {
    pub fn new(store: RateLimitStore) -> Self {
        Self { store }
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
    S: Service<Request<Body>, Response = Response<Body>>,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = Either<S::Future, Ready<Result<Response<Body>, S::Error>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        match self.store.check_request(&req, req.uri().path()) {
            Ok(()) => Either::Left(self.inner.call(req)),
            Err(rejection) => Either::Right(ready(Ok(rejection
                .into_error(crate::metrics::names::TRANSPORT_HTTP)
                .into_response()))),
        }
    }
}

/// Rate-limit keying needs a non-empty string per request; the
/// shared extractor's `None` becomes `"unknown"` here.
fn extract_client_ip<B>(req: &Request<B>) -> String {
    super::client_ip::extract_client_ip(req).unwrap_or_else(|| "unknown".to_string())
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
    use axum::extract::ConnectInfo;
    use axum::http::header::HeaderValue;
    use std::net::{IpAddr, SocketAddr};

    // Serializes the env-mutating `from_env` tests so they don't race the
    // shared process environment under the multi-threaded test runner. The
    // crate-wide lock (not a module-local one) because `GUARDIAN_MAX_REPLICAS`
    // is also mutated by the dashboard-config tests.
    use crate::testing::env_lock::ENV_LOCK;

    fn request_with_peer_ip(peer_ip: IpAddr) -> Request<Body> {
        let mut req = Request::builder().uri("/test").body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(peer_ip, 12345)));
        req
    }

    #[test]
    fn max_replicas_unset_is_one_and_invalid_is_rejected() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        // SAFETY: serialized by ENV_LOCK; vars are test-specific.
        unsafe {
            env::remove_var(ENV_MAX_REPLICAS);
        }
        assert_eq!(max_replicas_from_env(), Ok(1), "unset means one replica");

        // SAFETY: serialized by ENV_LOCK; vars are test-specific.
        unsafe {
            env::set_var(ENV_MAX_REPLICAS, " 6 ");
        }
        assert_eq!(max_replicas_from_env(), Ok(6), "whitespace is tolerated");

        for invalid in ["0", "six", "", "-2", "2.5"] {
            // SAFETY: serialized by ENV_LOCK; vars are test-specific.
            unsafe {
                env::set_var(ENV_MAX_REPLICAS, invalid);
            }
            assert!(
                max_replicas_from_env().is_err(),
                "{invalid:?} must be rejected, not silently treated as 1"
            );
        }

        // SAFETY: serialized by ENV_LOCK; vars are test-specific.
        unsafe {
            env::remove_var(ENV_MAX_REPLICAS);
        }
    }

    #[test]
    fn from_env_falls_back_to_no_partitioning_on_invalid_max_replicas() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        // SAFETY: serialized by ENV_LOCK; vars are test-specific.
        unsafe {
            env::set_var("GUARDIAN_RATE_BURST_PER_SEC", "600");
            env::set_var("GUARDIAN_RATE_PER_MIN", "6000");
            env::set_var(ENV_MAX_REPLICAS, "not-a-number");
        }

        let config = RateLimitConfig::from_env();

        // SAFETY: serialized by ENV_LOCK; vars are test-specific.
        unsafe {
            env::remove_var("GUARDIAN_RATE_BURST_PER_SEC");
            env::remove_var("GUARDIAN_RATE_PER_MIN");
            env::remove_var(ENV_MAX_REPLICAS);
        }

        // Non-prod behavior: warn and serve the unpartitioned global limit.
        // The prod builder guard refuses to start on the same input.
        assert_eq!(config.burst_per_sec, 600);
        assert_eq!(config.per_min, 6000);
    }

    #[test]
    fn partition_divides_global_limit_by_max_replicas() {
        assert_eq!(partition_limit(600, 6), 100);
        assert_eq!(partition_limit(600, 1), 600);
        assert_eq!(partition_limit(600, 0), 600, "zero replicas treated as one");
        // global < max_replicas: floor is 0 (deny) so the fleet aggregate
        // (6 x 0 = 0) never exceeds the global limit (FR-009).
        assert_eq!(partition_limit(5, 6), 0);
        // 6 x 100 = 600 == global; never exceeds.
        assert!(partition_limit(600, 6) * 6 <= 600);
    }

    #[test]
    fn test_rate_limit_config_default() {
        let config = RateLimitConfig::default();
        assert!(config.enabled);
        assert_eq!(config.burst_per_sec, DEFAULT_BURST_PER_SEC);
        assert_eq!(config.per_min, DEFAULT_PER_MIN);
    }

    #[test]
    fn test_rate_limit_config_new() {
        let config = RateLimitConfig::new(5, 30);
        assert!(config.enabled);
        assert_eq!(config.burst_per_sec, 5);
        assert_eq!(config.per_min, 30);
    }

    #[test]
    fn test_rate_limit_config_from_env_defaults() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        // SAFETY: serialized by ENV_LOCK; vars are test-specific.
        unsafe {
            env::remove_var(ENV_RATE_LIMIT_ENABLED);
            env::remove_var("GUARDIAN_RATE_BURST_PER_SEC");
            env::remove_var("GUARDIAN_RATE_PER_MIN");
            env::remove_var(ENV_MAX_REPLICAS);
        }

        let config = RateLimitConfig::from_env();
        assert!(config.enabled);
        assert_eq!(config.burst_per_sec, DEFAULT_BURST_PER_SEC);
        assert_eq!(config.per_min, DEFAULT_PER_MIN);
    }

    #[test]
    fn test_rate_limit_config_from_env_disabled() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        // SAFETY: serialized by ENV_LOCK; vars are test-specific.
        unsafe {
            env::set_var(ENV_RATE_LIMIT_ENABLED, "false");
        }

        let config = RateLimitConfig::from_env();
        assert!(!config.enabled);

        // SAFETY: serialized by ENV_LOCK; vars are test-specific.
        unsafe {
            env::remove_var(ENV_RATE_LIMIT_ENABLED);
        }
    }

    #[test]
    fn from_env_partitions_limits_by_max_replicas() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        // SAFETY: serialized by ENV_LOCK; vars are test-specific.
        unsafe {
            env::set_var("GUARDIAN_RATE_BURST_PER_SEC", "600");
            env::set_var("GUARDIAN_RATE_PER_MIN", "6000");
            env::set_var(ENV_MAX_REPLICAS, "6");
        }

        let config = RateLimitConfig::from_env();

        // SAFETY: serialized by ENV_LOCK; vars are test-specific.
        unsafe {
            env::remove_var("GUARDIAN_RATE_BURST_PER_SEC");
            env::remove_var("GUARDIAN_RATE_PER_MIN");
            env::remove_var(ENV_MAX_REPLICAS);
        }

        assert_eq!(config.burst_per_sec, 100);
        assert_eq!(config.per_min, 1000);
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
    fn test_rate_limit_store_burst_and_sustained_independent() {
        let config = RateLimitConfig::new(3, 5);
        let store = RateLimitStore::new(config);

        // Use up burst limit
        for _ in 0..3 {
            assert!(store.check_burst("independent_test").is_ok());
        }
        assert!(store.check_burst("independent_test").is_err());

        // Sustained should still work (different method, but same key creates new entry)
        // Note: Using different key to test sustained independently
        for _ in 0..5 {
            assert!(store.check_sustained("independent_test_sustained").is_ok());
        }
        assert!(store.check_sustained("independent_test_sustained").is_err());
    }

    #[test]
    fn test_rate_limit_store_zero_limits() {
        let config = RateLimitConfig::new(0, 0);
        let store = RateLimitStore::new(config);

        // With 0 limits, first request should fail
        assert!(store.check_burst("zero_test").is_err());
        assert!(store.check_sustained("zero_test").is_err());
    }

    // ================================================================================================
    // RateLimitType tests
    // ================================================================================================

    #[test]
    fn test_rate_limit_type_as_str() {
        assert_eq!(RateLimitType::Burst.as_str(), "burst");
        assert_eq!(RateLimitType::Sustained.as_str(), "sustained");
    }

    #[test]
    fn test_rate_limit_type_debug() {
        // Ensure Debug trait is implemented
        let burst = RateLimitType::Burst;
        let sustained = RateLimitType::Sustained;
        assert!(format!("{:?}", burst).contains("Burst"));
        assert!(format!("{:?}", sustained).contains("Sustained"));
    }

    #[test]
    fn test_rate_limit_type_clone() {
        let original = RateLimitType::Burst;
        let cloned = original;
        assert_eq!(original.as_str(), cloned.as_str());
    }

    #[tokio::test]
    async fn test_rate_limit_response_uses_canonical_error_envelope() {
        use axum::body::to_bytes;
        use axum::http::StatusCode;
        use axum::response::IntoResponse;

        let response = crate::error::GuardianError::RateLimitExceeded {
            retry_after_secs: 60,
            scope: "sustained".to_string(),
        }
        .into_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok()),
            Some("60")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // Canonical { code, message, meta } shape — no legacy success/error.
        assert!(parsed.get("success").is_none());
        assert!(parsed.get("error").is_none());
        assert_eq!(parsed["code"], "rate_limit_exceeded");
        assert!(parsed["message"].is_string());
        assert_eq!(parsed["meta"]["retryable"], serde_json::Value::Bool(true));
        assert_eq!(parsed["meta"]["retry_after_secs"], serde_json::json!(60));
    }

    #[test]
    fn test_rate_limit_layer_new() {
        let layer = RateLimitLayer::new(RateLimitStore::new(RateLimitConfig::new(10, 60)));
        assert!(format!("{:?}", layer).contains("RateLimitLayer"));
    }

    #[test]
    fn test_extract_client_ip_from_x_forwarded_for() {
        let mut req = request_with_peer_ip("10.10.10.10".parse().unwrap());

        req.headers_mut()
            .insert("x-forwarded-for", HeaderValue::from_static("192.168.1.100"));

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "192.168.1.100");
    }

    #[test]
    fn test_extract_client_ip_x_forwarded_for_uses_rightmost_entry() {
        let mut req = request_with_peer_ip("10.10.10.10".parse().unwrap());

        req.headers_mut().insert(
            "x-forwarded-for",
            HeaderValue::from_static("10.0.0.1, 192.168.1.1, 172.16.0.1"),
        );

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "172.16.0.1");
    }

    #[test]
    fn test_extract_client_ip_spoofed_prefix_does_not_change_key() {
        let mut req = request_with_peer_ip("10.10.10.10".parse().unwrap());

        req.headers_mut().insert(
            "x-forwarded-for",
            HeaderValue::from_static("6.6.6.6, 203.0.113.50"),
        );

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "203.0.113.50");
    }

    #[test]
    fn test_extract_client_ip_from_x_forwarded_for_with_spaces() {
        let mut req = request_with_peer_ip("10.10.10.10".parse().unwrap());

        req.headers_mut().insert(
            "x-forwarded-for",
            HeaderValue::from_static("  203.0.113.50  , 70.41.3.18  "),
        );

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "70.41.3.18");
    }

    #[test]
    fn test_extract_client_ip_multiple_headers_uses_last_entry_of_last_header() {
        // Multiple X-Forwarded-For header lines are equivalent to their
        // in-order comma-joined concatenation, so the rightmost entry of
        // the concatenation is the last entry of the last line.
        let mut req = request_with_peer_ip("10.10.10.10".parse().unwrap());

        req.headers_mut()
            .append("x-forwarded-for", HeaderValue::from_static("6.6.6.6"));
        req.headers_mut().append(
            "x-forwarded-for",
            HeaderValue::from_static("7.7.7.7, 203.0.113.50"),
        );

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "203.0.113.50");
    }

    #[test]
    fn test_extract_client_ip_unparseable_rightmost_entry_ignores_header() {
        let mut req = request_with_peer_ip("10.10.10.10".parse().unwrap());

        req.headers_mut().insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.50, not-an-ip"),
        );

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "10.10.10.10");
    }

    #[test]
    fn test_extract_client_ip_from_tonic_connect_info() {
        let mut req = Request::builder().uri("/test").body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(tonic::transport::server::TcpConnectInfo {
                local_addr: None,
                remote_addr: Some(SocketAddr::new("198.51.100.7".parse().unwrap(), 4444)),
            });

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "198.51.100.7");
    }

    #[test]
    fn test_extract_client_ip_from_x_real_ip() {
        let mut req = request_with_peer_ip("10.10.10.10".parse().unwrap());

        req.headers_mut()
            .insert("x-real-ip", HeaderValue::from_static("10.20.30.40"));

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "10.20.30.40");
    }

    #[test]
    fn test_extract_client_ip_x_forwarded_for_takes_precedence() {
        let mut req = request_with_peer_ip("10.10.10.10".parse().unwrap());

        // Both headers present - X-Forwarded-For should take precedence
        req.headers_mut()
            .insert("x-forwarded-for", HeaderValue::from_static("1.1.1.1"));
        req.headers_mut()
            .insert("x-real-ip", HeaderValue::from_static("2.2.2.2"));

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "1.1.1.1");
    }

    #[test]
    fn test_extract_client_ip_fallback_to_unknown() {
        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "unknown");
    }

    #[test]
    fn test_extract_client_ip_ipv6_from_header() {
        let mut req = request_with_peer_ip("10.10.10.10".parse().unwrap());

        req.headers_mut()
            .insert("x-forwarded-for", HeaderValue::from_static("2001:db8::1"));

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "2001:db8::1");
    }

    #[test]
    fn test_extract_client_ip_falls_back_to_peer_ip_without_headers() {
        let req = request_with_peer_ip("203.0.113.10".parse().unwrap());

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "203.0.113.10");
    }

    #[test]
    fn test_extract_client_ip_uses_peer_ip_when_no_forwarding_headers_exist() {
        let req = request_with_peer_ip("10.10.10.10".parse().unwrap());
        let ip = extract_client_ip(&req);
        assert_eq!(ip, "10.10.10.10");
    }

    #[test]
    fn test_extract_client_ip_uses_headers_even_without_connect_info() {
        let mut req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        req.headers_mut()
            .insert("x-forwarded-for", HeaderValue::from_static("192.168.1.100"));

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "192.168.1.100");
    }

    #[test]
    fn test_extract_client_ip_prefers_x_forwarded_for_over_peer_ip() {
        let mut req = request_with_peer_ip("10.10.10.10".parse().unwrap());

        req.headers_mut()
            .insert("x-forwarded-for", HeaderValue::from_static("192.168.1.100"));

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "192.168.1.100");
    }

    #[test]
    fn test_extract_enhanced_key_account_id_from_query() {
        let req = Request::builder()
            .uri("/delta?account_id=0x1234567890abcdef")
            .body(Body::empty())
            .unwrap();

        let key = extract_enhanced_key(&req);
        assert_eq!(key, Some("account:0x1234567890abcdef".to_string()));
    }

    #[test]
    fn test_extract_enhanced_key_account_id_with_other_params() {
        let req = Request::builder()
            .uri("/delta?nonce=5&account_id=0xabc123&other=value")
            .body(Body::empty())
            .unwrap();

        let key = extract_enhanced_key(&req);
        assert_eq!(key, Some("account:0xabc123".to_string()));
    }

    #[test]
    fn test_extract_enhanced_key_pubkey_from_header() {
        let mut req = Request::builder()
            .uri("/delta")
            .body(Body::empty())
            .unwrap();

        req.headers_mut().insert(
            "x-pubkey",
            HeaderValue::from_static("0x1234567890abcdef1234567890abcdef"),
        );

        let key = extract_enhanced_key(&req);
        // Should truncate to first 16 chars: "0x1234567890abcd"
        assert_eq!(key, Some("signer:0x1234567890abcd".to_string()));
    }

    #[test]
    fn test_extract_enhanced_key_short_pubkey() {
        let mut req = Request::builder()
            .uri("/delta")
            .body(Body::empty())
            .unwrap();

        req.headers_mut()
            .insert("x-pubkey", HeaderValue::from_static("short"));

        let key = extract_enhanced_key(&req);
        assert_eq!(key, Some("signer:short".to_string()));
    }

    #[test]
    fn test_extract_enhanced_key_account_id_takes_precedence() {
        let mut req = Request::builder()
            .uri("/delta?account_id=0xaccount123")
            .body(Body::empty())
            .unwrap();

        req.headers_mut()
            .insert("x-pubkey", HeaderValue::from_static("0xpubkey456"));

        let key = extract_enhanced_key(&req);
        // account_id should take precedence
        assert_eq!(key, Some("account:0xaccount123".to_string()));
    }

    #[test]
    fn test_extract_enhanced_key_none_when_no_identifiers() {
        let req = Request::builder()
            .uri("/pubkey")
            .body(Body::empty())
            .unwrap();

        let key = extract_enhanced_key(&req);
        assert_eq!(key, None);
    }

    #[test]
    fn test_extract_enhanced_key_empty_query_string() {
        let req = Request::builder()
            .uri("/delta?")
            .body(Body::empty())
            .unwrap();

        let key = extract_enhanced_key(&req);
        assert_eq!(key, None);
    }

    #[test]
    fn test_extract_enhanced_key_similar_param_name() {
        let req = Request::builder()
            .uri("/delta?account_id_backup=0x123&my_account_id=0x456")
            .body(Body::empty())
            .unwrap();

        // Should not match partial names
        let key = extract_enhanced_key(&req);
        assert_eq!(key, None);
    }

    #[test]
    fn test_rate_limit_key_generation() {
        // Test that different requests generate appropriate keys
        let config = RateLimitConfig::new(5, 30);
        let store = RateLimitStore::new(config);

        // Simulate key patterns that would be generated by the middleware
        let ip_key = "ip:192.168.1.1";
        let ip_endpoint_key = "ip:192.168.1.1|endpoint:/delta";
        let ip_account_key = "ip:192.168.1.1|account:0x123";

        // All should be independent
        for _ in 0..5 {
            assert!(store.check_burst(ip_key).is_ok());
            assert!(store.check_burst(ip_endpoint_key).is_ok());
            assert!(store.check_burst(ip_account_key).is_ok());
        }

        // Each should hit its own limit
        assert!(store.check_burst(ip_key).is_err());
        assert!(store.check_burst(ip_endpoint_key).is_err());
        assert!(store.check_burst(ip_account_key).is_err());
    }

    #[test]
    fn test_concurrent_store_access() {
        use std::thread;

        let config = RateLimitConfig::new(100, 1000);
        let store = RateLimitStore::new(config);

        let mut handles = vec![];

        // Spawn multiple threads accessing the store
        for i in 0..10 {
            let store_clone = store.clone();
            let handle = thread::spawn(move || {
                let key = format!("thread_{}", i);
                for _ in 0..10 {
                    let _ = store_clone.check_burst(&key);
                    let _ = store_clone.check_sustained(&key);
                }
            });
            handles.push(handle);
        }

        // All threads should complete without panic
        for handle in handles {
            handle.join().expect("Thread panicked");
        }
    }
}
