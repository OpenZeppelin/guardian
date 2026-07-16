use chrono::Duration;
use zeroize::Zeroizing;

use crate::dashboard::cursor::CursorSecret;
use crate::middleware::RateLimitConfig;
use crate::middleware::rate_limit::partition_limit;
use crate::network::NetworkType;

pub(crate) const OPEN_DASHBOARD_DOMAIN: &str = "*";
pub(crate) const DEFAULT_CANONICAL_DOMAIN: &str = OPEN_DASHBOARD_DOMAIN;
pub(crate) const DEFAULT_COOKIE_NAME: &str = "guardian_operator_session";
pub(crate) const DEFAULT_NONCE_TTL_SECS: i64 = 300;
pub(crate) const DEFAULT_SESSION_TTL_SECS: i64 = 8 * 60 * 60;
pub(crate) const DEFAULT_MAX_OUTSTANDING_CHALLENGES: usize = 8;
pub(crate) const DEFAULT_PUBKEY_RATE_BURST_PER_SEC: u32 = 6;
pub(crate) const DEFAULT_PUBKEY_RATE_PER_MIN: u32 = 30;
const ENV_COMMITMENT_RATE_BURST_PER_SEC: &str = "GUARDIAN_DASHBOARD_COMMITMENT_RATE_BURST_PER_SEC";
const ENV_COMMITMENT_RATE_PER_MIN: &str = "GUARDIAN_DASHBOARD_COMMITMENT_RATE_PER_MIN";
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
        let max_replicas = crate::middleware::rate_limit::max_replicas_from_env().unwrap_or(1);
        let commitment_rate_burst_per_sec = positive_u32_from_env(
            ENV_COMMITMENT_RATE_BURST_PER_SEC,
            DEFAULT_PUBKEY_RATE_BURST_PER_SEC,
        )?;
        let commitment_rate_per_min =
            positive_u32_from_env(ENV_COMMITMENT_RATE_PER_MIN, DEFAULT_PUBKEY_RATE_PER_MIN)?;
        let commitment_rate_limit = RateLimitConfig::new(
            partition_limit(commitment_rate_burst_per_sec, max_replicas).max(1),
            partition_limit(commitment_rate_per_min, max_replicas).max(1),
        );
        Ok(Self {
            environment: environment_for_network(network_type).to_string(),
            cursor_secret,
            commitment_rate_limit,
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

fn positive_u32_from_env(key: &str, default: u32) -> std::result::Result<u32, String> {
    match std::env::var(key) {
        Ok(raw) => match raw.trim().parse::<u32>() {
            Ok(0) => Err(format!("{key} must be a positive integer, got 0")),
            Ok(value) => Ok(value),
            Err(_) => Err(format!("{key} must be a positive integer, got {raw:?}")),
        },
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{key} must contain valid UTF-8")),
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
    use super::*;
    // Crate-wide env lock (not module-local): `GUARDIAN_MAX_REPLICAS` is also
    // mutated by the rate-limit middleware tests, and the process environment
    // is one shared global.
    use crate::testing::env_lock::ENV_LOCK;

    struct EnvVarGuard {
        previous: Vec<(&'static str, Option<String>)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvVarGuard {
        // secret-fields-allow: test-only env mutation guarded by ENV_LOCK
        fn set(key: &'static str, value: &str) -> Self {
            Self::set_all(&[(key, Some(value))])
        }

        fn remove(key: &'static str) -> Self {
            Self::set_all(&[(key, None)])
        }

        // secret-fields-allow: test-only env mutation guarded by ENV_LOCK
        fn set_all(values: &[(&'static str, Option<&str>)]) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
            let previous = values
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect();
            for (key, value) in values {
                match value {
                    Some(value) => unsafe { std::env::set_var(key, value) },
                    None => unsafe { std::env::remove_var(key) },
                }
            }
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            for (key, previous) in &self.previous {
                match previous {
                    Some(value) => unsafe { std::env::set_var(key, value) },
                    None => unsafe { std::env::remove_var(key) },
                }
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
    fn commitment_rate_limit_unpartitioned_for_single_replica() {
        let _guard = EnvVarGuard::remove("GUARDIAN_MAX_REPLICAS");
        let config =
            DashboardConfig::from_env_for_network(NetworkType::MidenTestnet).expect("config");
        assert_eq!(
            config.commitment_rate_limit.burst_per_sec,
            DEFAULT_PUBKEY_RATE_BURST_PER_SEC
        );
        assert_eq!(
            config.commitment_rate_limit.per_min,
            DEFAULT_PUBKEY_RATE_PER_MIN
        );
    }

    #[test]
    fn commitment_rate_limit_is_partitioned_by_max_replicas() {
        let _guard = EnvVarGuard::set("GUARDIAN_MAX_REPLICAS", "5");
        let config =
            DashboardConfig::from_env_for_network(NetworkType::MidenTestnet).expect("config");
        assert_eq!(config.commitment_rate_limit.burst_per_sec, 1); // 6 / 5
        assert_eq!(config.commitment_rate_limit.per_min, 6); // 30 / 5
    }

    #[test]
    fn commitment_rate_limit_is_partitioned_by_six_replicas() {
        let _guard = EnvVarGuard::set("GUARDIAN_MAX_REPLICAS", "6");
        let config =
            DashboardConfig::from_env_for_network(NetworkType::MidenTestnet).expect("config");
        assert_eq!(config.commitment_rate_limit.burst_per_sec, 1);
        assert_eq!(config.commitment_rate_limit.per_min, 5);
    }

    #[test]
    fn commitment_rate_limit_budgets_can_be_overridden() {
        let _guard = EnvVarGuard::set_all(&[
            ("GUARDIAN_MAX_REPLICAS", Some("6")),
            (ENV_COMMITMENT_RATE_BURST_PER_SEC, Some("12")),
            (ENV_COMMITMENT_RATE_PER_MIN, Some("60")),
        ]);
        let config =
            DashboardConfig::from_env_for_network(NetworkType::MidenTestnet).expect("config");
        assert_eq!(config.commitment_rate_limit.burst_per_sec, 2);
        assert_eq!(config.commitment_rate_limit.per_min, 10);
    }

    #[test]
    fn commitment_rate_limit_rejects_invalid_overrides() {
        let _guard = EnvVarGuard::set(ENV_COMMITMENT_RATE_PER_MIN, "not-a-number");
        let error = DashboardConfig::from_env_for_network(NetworkType::MidenTestnet)
            .expect_err("invalid rate must fail");
        assert!(error.contains(ENV_COMMITMENT_RATE_PER_MIN));
    }

    #[test]
    fn commitment_rate_limit_partition_clamps_to_at_least_one() {
        let _guard = EnvVarGuard::set("GUARDIAN_MAX_REPLICAS", "50");
        let config =
            DashboardConfig::from_env_for_network(NetworkType::MidenTestnet).expect("config");
        // Floor would be 0 (deny every login on this replica); the ≥1 clamp
        // keeps operator login alive at the cost of a replica-count-bounded
        // aggregate for a single commitment.
        assert_eq!(config.commitment_rate_limit.burst_per_sec, 1);
        assert_eq!(config.commitment_rate_limit.per_min, 1);
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
