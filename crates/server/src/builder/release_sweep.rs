//! Configuration of the chain-driven release sweep (issue #434).
//!
//! The sweep is its own background task, independent of the
//! canonicalization worker: release detection is not latency-sensitive
//! (an undetected switch costs stale reads and dead pending proposals,
//! never funds or custody), so it walks the fleet slowly at a bounded
//! RPC rate instead of squeezing pages between candidate passes. See
//! `jobs::release_sweep` for the walk itself.

use std::time::Duration;

/// Environment override for [`ReleaseSweepConfig::enabled`]. `false` is
/// the runtime kill switch: release detection then relies on the push
/// path alone.
pub const ENV_ENABLED: &str = "GUARDIAN_RELEASE_SWEEP_ENABLED";

/// Environment override for [`ReleaseSweepConfig::rotation_seconds`].
pub const ENV_ROTATION_SECONDS: &str = "GUARDIAN_RELEASE_SWEEP_ROTATION_SECONDS";

/// Environment override for [`ReleaseSweepConfig::max_rate_per_second`].
pub const ENV_MAX_RATE_PER_SECOND: &str = "GUARDIAN_RELEASE_SWEEP_MAX_RATE_PER_SECOND";

/// Environment override for [`ReleaseSweepConfig::page_size`].
pub const ENV_PAGE_SIZE: &str = "GUARDIAN_RELEASE_SWEEP_PAGE_SIZE";

/// Environment override for [`ReleaseSweepConfig::hot_interval_seconds`].
pub const ENV_HOT_INTERVAL_SECONDS: &str = "GUARDIAN_RELEASE_SWEEP_HOT_INTERVAL_SECONDS";

/// Environment override for [`ReleaseSweepConfig::confirmations`].
pub const ENV_CONFIRMATIONS: &str = "GUARDIAN_RELEASE_SWEEP_CONFIRMATIONS";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseSweepConfig {
    /// Whether the sweep task runs at all.
    pub enabled: bool,

    /// Target time for one full walk of the fleet: every unreleased
    /// Miden account without a candidate in flight is probed once per
    /// rotation. The walk is paced so that it spreads over this window
    /// (never faster than `max_rate_per_second`), then the next rotation
    /// starts when the window elapses.
    pub rotation_seconds: u64,

    /// Upper bound on accounts probed per second during the walk, i.e.
    /// the sweep's share of chain-node RPC capacity. A small fleet
    /// finishes its rotation early and idles; a large one is bounded by
    /// this rate rather than by the rotation target.
    pub max_rate_per_second: u32,

    /// Accounts fetched from the metadata store per listing page. Only
    /// a batching detail of the walk: the cursor advances per visited
    /// account, never per page.
    pub page_size: u32,

    /// Cadence of the hot pass, which re-probes the small set of
    /// accounts that need attention sooner than the next rotation:
    /// accounts with an open confirmation streak and accounts with a
    /// pending `switch_guardian` proposal.
    pub hot_interval_seconds: u64,

    /// Consecutive observations of a foreign guardian key in published
    /// on-chain storage required before an account is released on that
    /// evidence. Values above 1 shield against a single stale RPC read
    /// (a lagging node serving a state from before a switch-back).
    /// An exact match against a pending switch proposal's precomputed
    /// post-state is proof and never waits for confirmation.
    pub confirmations: u32,
}

impl Default for ReleaseSweepConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rotation_seconds: 6 * 60 * 60, // every account checked every 6 hours
            max_rate_per_second: 5,        // bounded share of node RPC capacity
            page_size: 100,
            hot_interval_seconds: 60,
            confirmations: 2,
        }
    }
}

impl ReleaseSweepConfig {
    pub fn rotation(&self) -> Duration {
        Duration::from_secs(self.rotation_seconds)
    }

    pub fn hot_interval(&self) -> Duration {
        Duration::from_secs(self.hot_interval_seconds)
    }

    /// Minimum spacing between two probed accounts at `max_rate_per_second`.
    pub fn min_spacing(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.max_rate_per_second.max(1)))
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn with_rotation_seconds(mut self, seconds: u64) -> Self {
        assert!(
            seconds > 0,
            "release sweep rotation must be at least one second"
        );
        self.rotation_seconds = seconds;
        self
    }

    pub fn with_max_rate_per_second(mut self, rate: u32) -> Self {
        assert!(
            rate > 0,
            "release sweep rate must be at least one account per second"
        );
        self.max_rate_per_second = rate;
        self
    }

    pub fn with_page_size(mut self, accounts: u32) -> Self {
        assert!(
            accounts > 0,
            "release sweep page size must be at least one account"
        );
        self.page_size = accounts;
        self
    }

    pub fn with_hot_interval_seconds(mut self, seconds: u64) -> Self {
        assert!(
            seconds > 0,
            "release sweep hot interval must be at least one second"
        );
        self.hot_interval_seconds = seconds;
        self
    }

    pub fn with_confirmations(mut self, confirmations: u32) -> Self {
        assert!(
            confirmations > 0,
            "release sweep confirmations must be at least one"
        );
        self.confirmations = confirmations;
        self
    }

    /// Defaults with every `GUARDIAN_RELEASE_SWEEP_*` override applied.
    /// A present-but-invalid value fails startup loudly.
    pub fn from_env() -> Result<Self, String> {
        Self::default().with_env_overrides(
            ENV_ENABLED,
            ENV_ROTATION_SECONDS,
            ENV_MAX_RATE_PER_SECOND,
            ENV_PAGE_SIZE,
            ENV_HOT_INTERVAL_SECONDS,
            ENV_CONFIRMATIONS,
        )
    }

    fn with_env_overrides(
        self,
        enabled_var: &str,
        rotation_var: &str,
        rate_var: &str,
        page_var: &str,
        hot_var: &str,
        confirmations_var: &str,
    ) -> Result<Self, String> {
        let mut config = self;
        if let Some(enabled) = bool_from_var(enabled_var)? {
            config = config.with_enabled(enabled);
        }
        if let Some(seconds) = positive_u64_from_var(rotation_var)? {
            config = config.with_rotation_seconds(seconds);
        }
        if let Some(rate) = positive_u64_from_var(rate_var)? {
            let rate = u32::try_from(rate).map_err(|_| format!("{rate_var} is too large"))?;
            config = config.with_max_rate_per_second(rate);
        }
        if let Some(accounts) = positive_u64_from_var(page_var)? {
            let accounts =
                u32::try_from(accounts).map_err(|_| format!("{page_var} is too large"))?;
            config = config.with_page_size(accounts);
        }
        if let Some(seconds) = positive_u64_from_var(hot_var)? {
            config = config.with_hot_interval_seconds(seconds);
        }
        if let Some(confirmations) = positive_u64_from_var(confirmations_var)? {
            let confirmations = u32::try_from(confirmations)
                .map_err(|_| format!("{confirmations_var} is too large"))?;
            config = config.with_confirmations(confirmations);
        }
        Ok(config)
    }
}

fn bool_from_var(var_name: &str) -> Result<Option<bool>, String> {
    match std::env::var(var_name) {
        Ok(value) => value
            .parse::<bool>()
            .map(Some)
            .map_err(|_| format!("{var_name} must be 'true' or 'false', got '{value}'")),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{var_name} contains invalid UTF-8")),
    }
}

fn positive_u64_from_var(var_name: &str) -> Result<Option<u64>, String> {
    match std::env::var(var_name) {
        Ok(value) => {
            let parsed = value
                .parse::<u64>()
                .map_err(|_| format!("{var_name} must be a positive integer, got '{value}'"))?;
            if parsed == 0 {
                return Err(format!("{var_name} must be greater than zero"));
            }
            Ok(Some(parsed))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{var_name} contains invalid UTF-8")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::env_lock::ENV_LOCK;

    const VARS: [&str; 6] = [
        "GUARDIAN_RS_TEST_ENABLED",
        "GUARDIAN_RS_TEST_ROTATION",
        "GUARDIAN_RS_TEST_RATE",
        "GUARDIAN_RS_TEST_PAGE",
        "GUARDIAN_RS_TEST_HOT",
        "GUARDIAN_RS_TEST_CONFIRMATIONS",
    ];

    fn from_test_vars() -> Result<ReleaseSweepConfig, String> {
        ReleaseSweepConfig::default()
            .with_env_overrides(VARS[0], VARS[1], VARS[2], VARS[3], VARS[4], VARS[5])
    }

    fn clear_vars() {
        for var in VARS {
            unsafe { std::env::remove_var(var) };
        }
    }

    #[test]
    fn defaults_are_slow_bounded_and_on() {
        let config = ReleaseSweepConfig::default();
        assert!(config.enabled);
        assert_eq!(config.rotation_seconds, 21_600);
        assert_eq!(config.max_rate_per_second, 5);
        assert_eq!(config.page_size, 100);
        assert_eq!(config.hot_interval_seconds, 60);
        assert_eq!(config.confirmations, 2);
        assert_eq!(config.min_spacing(), Duration::from_millis(200));
    }

    #[test]
    fn env_overrides_apply_and_missing_keeps_defaults() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear_vars();
        assert_eq!(from_test_vars().unwrap(), ReleaseSweepConfig::default());

        unsafe {
            std::env::set_var(VARS[0], "false");
            std::env::set_var(VARS[1], "3600");
            std::env::set_var(VARS[2], "20");
            std::env::set_var(VARS[3], "250");
            std::env::set_var(VARS[4], "30");
            std::env::set_var(VARS[5], "1");
        }
        let config = from_test_vars().expect("valid overrides apply");
        assert!(!config.enabled, "false is the kill switch");
        assert_eq!(config.rotation_seconds, 3600);
        assert_eq!(config.max_rate_per_second, 20);
        assert_eq!(config.page_size, 250);
        assert_eq!(config.hot_interval_seconds, 30);
        assert_eq!(config.confirmations, 1);
        clear_vars();
    }

    #[test]
    fn env_overrides_reject_zero_and_garbage() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear_vars();
        for (var, bad) in [
            (VARS[0], "0"),
            (VARS[1], "0"),
            (VARS[2], "fast"),
            (VARS[3], "0"),
            (VARS[4], "-1"),
            (VARS[5], "0"),
        ] {
            unsafe { std::env::set_var(var, bad) };
            assert!(from_test_vars().is_err(), "{var}={bad} must fail startup");
            unsafe { std::env::remove_var(var) };
        }
    }
}
