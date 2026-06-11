//! Environment-driven configuration for the Prometheus metrics
//! integration. Mirrors the `RateLimitConfig` pattern: a plain struct
//! with a `from_env()` constructor and lenient parsing (invalid values
//! fall back to defaults with a warning).

use std::env;
use std::net::SocketAddr;
use std::time::Duration;

use crate::middleware::rate_limit::env_flag;
use crate::secret::SecretString;

/// Master switch. Metrics are opt-in: a custody service should not
/// open a new listener unless the operator asked for one.
const ENV_METRICS_ENABLED: &str = "GUARDIAN_METRICS_ENABLED";
const ENV_METRICS_ADDR: &str = "GUARDIAN_METRICS_ADDR";
const ENV_METRICS_PATH: &str = "GUARDIAN_METRICS_PATH";
const ENV_METRICS_REFRESH_INTERVAL_SECS: &str = "GUARDIAN_METRICS_REFRESH_INTERVAL_SECS";
const ENV_METRICS_BEARER_TOKEN: &str = "GUARDIAN_METRICS_BEARER_TOKEN";

/// Loopback by default: the metrics listener is reachable only from
/// the host unless an operator deliberately binds a routable address
/// (containers set `0.0.0.0:9464`). 9464 is the conventional
/// Prometheus exporter port for application metrics.
const DEFAULT_METRICS_ADDR: &str = "127.0.0.1:9464";
const DEFAULT_METRICS_PATH: &str = "/metrics";
const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 30;

/// Metrics configuration loaded from environment
#[derive(Debug, Clone)]
pub struct MetricsConfig {
    /// Whether the metrics listener, recorder, and refresher run at all
    pub enabled: bool,
    /// Bind address of the dedicated metrics listener
    pub bind_addr: SocketAddr,
    /// Path serving the Prometheus exposition (must start with `/`)
    pub path: String,
    /// Cadence of the slow-aggregate background refresher
    pub refresh_interval: Duration,
    /// Optional shared-secret scrape token. When set, requests must
    /// carry `Authorization: Bearer <token>` or receive `401`.
    /// `SecretString` per the crate's secret-hygiene rule: never
    /// logged, never serialized, zeroed on drop.
    pub(crate) bearer_token: Option<SecretString>,
}

impl MetricsConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Self {
        let enabled = env_flag(ENV_METRICS_ENABLED, false);

        let bind_addr = match env::var(ENV_METRICS_ADDR) {
            Ok(raw) => raw.parse().unwrap_or_else(|_| {
                tracing::warn!(
                    value = %raw,
                    default = DEFAULT_METRICS_ADDR,
                    "Invalid GUARDIAN_METRICS_ADDR; falling back to default"
                );
                Self::default_addr()
            }),
            Err(_) => Self::default_addr(),
        };

        let path = match env::var(ENV_METRICS_PATH) {
            Ok(raw) if raw.starts_with('/') => raw,
            Ok(raw) => {
                tracing::warn!(
                    value = %raw,
                    default = DEFAULT_METRICS_PATH,
                    "GUARDIAN_METRICS_PATH must start with '/'; falling back to default"
                );
                DEFAULT_METRICS_PATH.to_string()
            }
            Err(_) => DEFAULT_METRICS_PATH.to_string(),
        };

        let refresh_interval = Duration::from_secs(
            env::var(ENV_METRICS_REFRESH_INTERVAL_SECS)
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|secs| *secs > 0)
                .unwrap_or(DEFAULT_REFRESH_INTERVAL_SECS),
        );

        // Read-and-wrap in one expression so the plaintext token never
        // lives in a local binding outside the wrapper.
        let bearer_token = env::var(ENV_METRICS_BEARER_TOKEN)
            .ok()
            .filter(|token| !token.is_empty())
            .map(SecretString::new);

        Self {
            enabled,
            bind_addr,
            path,
            refresh_interval,
            bearer_token,
        }
    }

    /// Replace the scrape token (used by tests and embedders that
    /// configure the server programmatically; `SecretString` is
    /// crate-private so the setter takes a plain `String`).
    pub fn with_bearer_token(mut self, token: String) -> Self {
        self.bearer_token = Some(SecretString::new(token));
        self
    }

    fn default_addr() -> SocketAddr {
        DEFAULT_METRICS_ADDR
            .parse()
            .expect("default metrics address is valid")
    }
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_addr: Self::default_addr(),
            path: DEFAULT_METRICS_PATH.to_string(),
            refresh_interval: Duration::from_secs(DEFAULT_REFRESH_INTERVAL_SECS),
            bearer_token: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// Tests in this module mutate the same process-global env vars;
    /// serialize them so parallel test threads don't race.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn clear_env() {
        // SAFETY: tests in this module touch only metrics-specific env
        // vars; mirrors the rate_limit.rs test pattern.
        unsafe {
            env::remove_var(ENV_METRICS_ENABLED);
            env::remove_var(ENV_METRICS_ADDR);
            env::remove_var(ENV_METRICS_PATH);
            env::remove_var(ENV_METRICS_REFRESH_INTERVAL_SECS);
            env::remove_var(ENV_METRICS_BEARER_TOKEN);
        }
    }

    #[test]
    fn from_env_defaults() {
        let _guard = lock_env();
        clear_env();
        let config = MetricsConfig::from_env();
        assert!(!config.enabled, "metrics must be opt-in");
        assert_eq!(config.bind_addr.to_string(), "127.0.0.1:9464");
        assert_eq!(config.path, "/metrics");
        assert_eq!(config.refresh_interval, Duration::from_secs(30));
        assert!(config.bearer_token.is_none());
    }

    #[test]
    fn from_env_reads_values() {
        let _guard = lock_env();
        clear_env();
        // SAFETY: see clear_env
        unsafe {
            env::set_var(ENV_METRICS_ENABLED, "true");
            env::set_var(ENV_METRICS_ADDR, "0.0.0.0:9999");
            env::set_var(ENV_METRICS_PATH, "/internal/metrics");
            env::set_var(ENV_METRICS_REFRESH_INTERVAL_SECS, "5");
            env::set_var(ENV_METRICS_BEARER_TOKEN, "scrape-secret");
        }

        let config = MetricsConfig::from_env();
        assert!(config.enabled);
        assert_eq!(config.bind_addr.to_string(), "0.0.0.0:9999");
        assert_eq!(config.path, "/internal/metrics");
        assert_eq!(config.refresh_interval, Duration::from_secs(5));
        assert_eq!(
            config.bearer_token.as_ref().map(|t| t.expose_secret()),
            Some("scrape-secret")
        );

        clear_env();
    }

    #[test]
    fn invalid_addr_and_path_fall_back_to_defaults() {
        let _guard = lock_env();
        clear_env();
        // SAFETY: see clear_env
        unsafe {
            env::set_var(ENV_METRICS_ADDR, "not-an-addr");
            env::set_var(ENV_METRICS_PATH, "missing-slash");
            env::set_var(ENV_METRICS_REFRESH_INTERVAL_SECS, "0");
        }

        let config = MetricsConfig::from_env();
        assert_eq!(config.bind_addr.to_string(), "127.0.0.1:9464");
        assert_eq!(config.path, "/metrics");
        assert_eq!(config.refresh_interval, Duration::from_secs(30));

        clear_env();
    }

    #[test]
    fn empty_bearer_token_counts_as_unset() {
        let _guard = lock_env();
        clear_env();
        // SAFETY: see clear_env
        unsafe {
            env::set_var(ENV_METRICS_BEARER_TOKEN, "");
        }
        assert!(MetricsConfig::from_env().bearer_token.is_none());
        clear_env();
    }
}
