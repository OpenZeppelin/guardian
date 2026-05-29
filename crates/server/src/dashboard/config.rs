use chrono::Duration;
use zeroize::Zeroizing;

use crate::dashboard::cursor::CursorSecret;
use crate::middleware::RateLimitConfig;
use crate::network::NetworkType;

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
/// `GET /dashboard/info`.
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
    /// Optional pre-parsed HMAC secret for the dashboard cursor codec.
    /// When `None`, [`DashboardState`] generates a fresh random secret
    /// per process — fine for single-replica deployments and unit
    /// tests; multi-replica deployments must pin a shared secret here
    /// so cursors validate across replicas. Sourced from
    /// `GUARDIAN_DASHBOARD_CURSOR_SECRET` (parsed at config-load time
    /// so no intermediate `String` lives in the config).
    pub(crate) cursor_secret: Option<CursorSecret>,
}

impl DashboardConfig {
    pub fn from_env_for_network(network_type: NetworkType) -> std::result::Result<Self, String> {
        let cursor_secret = std::env::var("GUARDIAN_DASHBOARD_CURSOR_SECRET")
            .ok()
            .map(Zeroizing::new)
            .map(|hex| CursorSecret::from_hex(hex.as_str()))
            .transpose()
            .map_err(|e| {
                format!(
                    "GUARDIAN_DASHBOARD_CURSOR_SECRET must be 32 hex-encoded bytes (64 chars): {e}"
                )
            })?;
        Ok(Self {
            environment: environment_for_network(network_type).to_string(),
            cursor_secret,
            ..Self::default()
        })
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

    pub(crate) fn take_cursor_secret(&mut self) -> Option<CursorSecret> {
        self.cursor_secret.take()
    }
}

fn environment_for_network(network_type: NetworkType) -> &'static str {
    match network_type {
        NetworkType::MidenTestnet => "testnet",
        NetworkType::MidenDevnet => "devnet",
        NetworkType::MidenLocal => "local",
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
            cursor_secret: None,
        }
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use std::sync::{LazyLock, Mutex};

    use super::*;

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvVarGuard {
        // secret-fields-allow: test-only env mutation guarded by ENV_LOCK
        fn set(key: &'static str, value: &str) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
            let previous = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self {
                key,
                previous,
                _lock: lock,
            }
        }

        fn remove(key: &'static str) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
            let previous = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            Self {
                key,
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

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

    #[test]
    fn from_env_parses_valid_cursor_secret_hex() {
        let _guard = EnvVarGuard::set(
            "GUARDIAN_DASHBOARD_CURSOR_SECRET",
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
        );
        let mut config =
            DashboardConfig::from_env_for_network(NetworkType::MidenTestnet).expect("parses");
        assert!(config.take_cursor_secret().is_some());
        assert!(
            config.take_cursor_secret().is_none(),
            "take_cursor_secret must be one-shot"
        );
    }

    #[test]
    fn from_env_rejects_invalid_cursor_secret_hex() {
        let _guard = EnvVarGuard::set("GUARDIAN_DASHBOARD_CURSOR_SECRET", "not-hex");
        let err = DashboardConfig::from_env_for_network(NetworkType::MidenTestnet)
            .expect_err("invalid hex must error");
        assert!(
            err.contains("GUARDIAN_DASHBOARD_CURSOR_SECRET"),
            "error must name the env var: {err}"
        );
    }

    #[test]
    fn environment_is_derived_from_network_type() {
        let _cursor = EnvVarGuard::remove("GUARDIAN_DASHBOARD_CURSOR_SECRET");
        assert_eq!(
            DashboardConfig::from_env_for_network(NetworkType::MidenTestnet)
                .unwrap()
                .environment(),
            "testnet"
        );
        assert_eq!(
            DashboardConfig::from_env_for_network(NetworkType::MidenDevnet)
                .unwrap()
                .environment(),
            "devnet"
        );
        assert_eq!(
            DashboardConfig::from_env_for_network(NetworkType::MidenLocal)
                .unwrap()
                .environment(),
            "local"
        );
    }
}
