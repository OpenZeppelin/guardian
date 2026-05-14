use chrono::Duration;

use crate::middleware::RateLimitConfig;

pub(crate) const OPEN_DASHBOARD_DOMAIN: &str = "*";
pub(crate) const DEFAULT_CANONICAL_DOMAIN: &str = OPEN_DASHBOARD_DOMAIN;
pub(crate) const DEFAULT_COOKIE_NAME: &str = "guardian_operator_session";
pub(crate) const DEFAULT_NONCE_TTL_SECS: i64 = 300;
pub(crate) const DEFAULT_SESSION_TTL_SECS: i64 = 8 * 60 * 60;
pub(crate) const DEFAULT_MAX_OUTSTANDING_CHALLENGES: usize = 8;
pub(crate) const DEFAULT_PUBKEY_RATE_BURST_PER_SEC: u32 = 5;
pub(crate) const DEFAULT_PUBKEY_RATE_PER_MIN: u32 = 30;
/// Default account-count threshold above which dashboard cross-account
/// aggregates may return a degraded marker on filesystem-backed
/// deployments, per FR-029 of `005-operator-dashboard-metrics`.
pub(crate) const DEFAULT_FILESYSTEM_AGGREGATE_THRESHOLD: usize = 1_000;
/// Default deployment environment identifier exposed on
/// `GET /dashboard/info`. Operators set `GUARDIAN_ENVIRONMENT` to
/// override (e.g. `mainnet`, `testnet`, `staging`).
pub(crate) const DEFAULT_ENVIRONMENT: &str = "testnet";

#[derive(Clone, Debug)]
pub struct DashboardConfig {
    pub(crate) canonical_domain: String,
    pub(crate) cookie_name: String,
    pub(crate) nonce_ttl: Duration,
    pub(crate) session_ttl: Duration,
    pub(crate) max_outstanding_challenges: usize,
    pub(crate) commitment_rate_limit: RateLimitConfig,
    pub(crate) filesystem_aggregate_threshold: usize,
    pub(crate) environment: String,
    /// Optional 32-byte hex-encoded HMAC secret for the dashboard
    /// cursor codec. When `None`, [`DashboardState`] generates a fresh
    /// random secret per process — fine for single-replica
    /// deployments and unit tests; multi-replica deployments must
    /// pin a shared secret here so cursors validate across replicas.
    /// Sourced from `GUARDIAN_DASHBOARD_CURSOR_SECRET`.
    pub(crate) cursor_secret_hex: Option<String>,
}

impl DashboardConfig {
    pub fn from_env() -> std::result::Result<Self, String> {
        let mut config = Self::default();
        if let Ok(env) = std::env::var("GUARDIAN_ENVIRONMENT") {
            config.environment = env;
        }
        if let Ok(secret_hex) = std::env::var("GUARDIAN_DASHBOARD_CURSOR_SECRET") {
            config.cursor_secret_hex = Some(secret_hex);
        }
        Ok(config)
    }

    pub fn for_tests() -> Self {
        Self::default()
    }

    pub(crate) fn filesystem_aggregate_threshold(&self) -> usize {
        self.filesystem_aggregate_threshold
    }

    pub(crate) fn environment(&self) -> &str {
        &self.environment
    }

    pub(crate) fn cursor_secret_hex(&self) -> Option<&str> {
        self.cursor_secret_hex.as_deref()
    }
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            canonical_domain: DEFAULT_CANONICAL_DOMAIN.to_string(),
            cookie_name: DEFAULT_COOKIE_NAME.to_string(),
            nonce_ttl: Duration::seconds(DEFAULT_NONCE_TTL_SECS),
            session_ttl: Duration::seconds(DEFAULT_SESSION_TTL_SECS),
            max_outstanding_challenges: DEFAULT_MAX_OUTSTANDING_CHALLENGES,
            commitment_rate_limit: RateLimitConfig {
                enabled: true,
                burst_per_sec: DEFAULT_PUBKEY_RATE_BURST_PER_SEC,
                per_min: DEFAULT_PUBKEY_RATE_PER_MIN,
            },
            filesystem_aggregate_threshold: DEFAULT_FILESYSTEM_AGGREGATE_THRESHOLD,
            environment: DEFAULT_ENVIRONMENT.to_string(),
            cursor_secret_hex: None,
        }
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;

    #[test]
    fn default_filesystem_aggregate_threshold_is_1000() {
        let config = DashboardConfig::default();
        assert_eq!(config.filesystem_aggregate_threshold(), 1_000);
    }

    #[test]
    fn filesystem_aggregate_threshold_can_be_overridden() {
        let config = DashboardConfig {
            filesystem_aggregate_threshold: 5_000,
            ..DashboardConfig::default()
        };
        assert_eq!(config.filesystem_aggregate_threshold(), 5_000);
    }

    #[test]
    fn for_tests_uses_default_threshold() {
        let config = DashboardConfig::for_tests();
        assert_eq!(
            config.filesystem_aggregate_threshold(),
            DEFAULT_FILESYSTEM_AGGREGATE_THRESHOLD
        );
    }
}
