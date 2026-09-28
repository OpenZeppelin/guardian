//! Configuration of the chain-driven release sweep (issue #434).
//!
//! The sweep is its own background task, independent of the
//! canonicalization worker: release detection is not latency-sensitive
//! (an undetected switch costs stale reads and dead pending proposals,
//! never funds or custody), so it walks the fleet slowly, one paced
//! visit at a time, instead of squeezing pages between candidate passes.
//! See `jobs::release_sweep` for the walk itself.

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

/// Environment override for [`ReleaseSweepConfig::recheck_seconds`].
pub const ENV_RECHECK_SECONDS: &str = "GUARDIAN_RELEASE_SWEEP_RECHECK_SECONDS";

/// Environment override for [`ReleaseSweepConfig::confirmations`].
pub const ENV_CONFIRMATIONS: &str = "GUARDIAN_RELEASE_SWEEP_CONFIRMATIONS";

/// Upper bounds of the settings: far beyond any sensible deployment, and
/// small enough that every deadline the sweep derives stays representable.
pub const MAX_ROTATION_SECONDS: u64 = 30 * 24 * 60 * 60;
pub const MAX_RECHECK_SECONDS: u64 = 24 * 60 * 60;
pub const MAX_RATE_PER_SECOND: u32 = 1_000;
pub const MAX_PAGE_SIZE: u32 = 10_000;
pub const MAX_CONFIRMATIONS: u32 = 100;

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

    /// Upper bound on account visits per second, rotation and
    /// confirmation re-checks together, i.e. the sweep's share of
    /// chain-node RPC capacity. A visit is one `GetAccount`; only for an
    /// account whose chain state moved past the stored one it adds a
    /// storage read and, per candidate that moves the guardian key away,
    /// a chain-tip read and at least one `SyncTransactions` page. A walk
    /// spreads over the rotation window whatever the fleet size; a fleet
    /// too large to fit it at this rate takes longer instead.
    pub max_rate_per_second: u32,

    /// Accounts fetched from the metadata store per listing page. Only
    /// a batching detail of the walk: the cursor advances per visited
    /// account, never per page.
    pub page_size: u32,

    /// Delay between confirmation re-checks of an account whose
    /// published storage showed a foreign guardian key. Re-checks share
    /// the rotation's pacing, so they never raise the visit rate.
    pub recheck_seconds: u64,

    /// Observations of the same foreign guardian key in published
    /// on-chain storage required before an account is released on that
    /// evidence, each at a strictly later block than the one before (the
    /// rotation visit is the first, re-checks follow `recheck_seconds`
    /// apart). A read of a state older than the stored one never counts,
    /// whatever this value. A pending proposal or unpromoted delta whose
    /// post-state is found on chain is proof and never waits for
    /// confirmation.
    pub confirmations: u32,
}

impl Default for ReleaseSweepConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rotation_seconds: 6 * 60 * 60, // every account checked every 6 hours
            max_rate_per_second: 5,        // bounded share of node RPC capacity
            page_size: 100,
            recheck_seconds: 60,
            confirmations: 2,
        }
    }
}

impl ReleaseSweepConfig {
    pub fn rotation(&self) -> Duration {
        Duration::from_secs(self.rotation_seconds)
    }

    pub fn recheck(&self) -> Duration {
        Duration::from_secs(self.recheck_seconds)
    }

    /// Minimum spacing between two account visits at `max_rate_per_second`.
    pub fn min_spacing(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.max_rate_per_second.max(1)))
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn with_rotation_seconds(mut self, seconds: u64) -> Self {
        assert!(
            (1..=MAX_ROTATION_SECONDS).contains(&seconds),
            "release sweep rotation must be between 1 and {MAX_ROTATION_SECONDS} seconds"
        );
        self.rotation_seconds = seconds;
        self
    }

    pub fn with_max_rate_per_second(mut self, rate: u32) -> Self {
        assert!(
            (1..=MAX_RATE_PER_SECOND).contains(&rate),
            "release sweep rate must be between 1 and {MAX_RATE_PER_SECOND} accounts per second"
        );
        self.max_rate_per_second = rate;
        self
    }

    pub fn with_page_size(mut self, accounts: u32) -> Self {
        assert!(
            (1..=MAX_PAGE_SIZE).contains(&accounts),
            "release sweep page size must be between 1 and {MAX_PAGE_SIZE} accounts"
        );
        self.page_size = accounts;
        self
    }

    pub fn with_recheck_seconds(mut self, seconds: u64) -> Self {
        assert!(
            (1..=MAX_RECHECK_SECONDS).contains(&seconds),
            "release sweep re-check delay must be between 1 and {MAX_RECHECK_SECONDS} seconds"
        );
        self.recheck_seconds = seconds;
        self
    }

    pub fn with_confirmations(mut self, confirmations: u32) -> Self {
        assert!(
            (1..=MAX_CONFIRMATIONS).contains(&confirmations),
            "release sweep confirmations must be between 1 and {MAX_CONFIRMATIONS}"
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
            ENV_RECHECK_SECONDS,
            ENV_CONFIRMATIONS,
        )
    }

    fn with_env_overrides(
        self,
        enabled_var: &str,
        rotation_var: &str,
        rate_var: &str,
        page_var: &str,
        recheck_var: &str,
        confirmations_var: &str,
    ) -> Result<Self, String> {
        let mut config = self;
        if let Some(enabled) = bool_from_var(enabled_var)? {
            config = config.with_enabled(enabled);
        }
        if let Some(seconds) = bounded_from_var(rotation_var, MAX_ROTATION_SECONDS)? {
            config = config.with_rotation_seconds(seconds);
        }
        if let Some(rate) = bounded_from_var(rate_var, u64::from(MAX_RATE_PER_SECOND))? {
            config = config.with_max_rate_per_second(to_u32(rate));
        }
        if let Some(accounts) = bounded_from_var(page_var, u64::from(MAX_PAGE_SIZE))? {
            config = config.with_page_size(to_u32(accounts));
        }
        if let Some(seconds) = bounded_from_var(recheck_var, MAX_RECHECK_SECONDS)? {
            config = config.with_recheck_seconds(seconds);
        }
        if let Some(confirmations) =
            bounded_from_var(confirmations_var, u64::from(MAX_CONFIRMATIONS))?
        {
            config = config.with_confirmations(to_u32(confirmations));
        }
        Ok(config)
    }
}

fn bool_from_var(var_name: &str) -> Result<Option<bool>, String> {
    match std::env::var(var_name) {
        Ok(value) => value
            .trim()
            .parse::<bool>()
            .map(Some)
            .map_err(|_| format!("{var_name} must be 'true' or 'false', got {value:?}")),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(format!("{var_name} must contain valid UTF-8"))
        }
    }
}

/// An integer in `1..=max`, or `None` when the variable is unset.
fn bounded_from_var(var_name: &str, max: u64) -> Result<Option<u64>, String> {
    match std::env::var(var_name) {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(parsed) if (1..=max).contains(&parsed) => Ok(Some(parsed)),
            _ => Err(format!(
                "{var_name} must be an integer between 1 and {max}, got {value:?}"
            )),
        },
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(format!("{var_name} must contain valid UTF-8"))
        }
    }
}

/// Every `u32` setting's bound fits in a `u32`.
fn to_u32(value: u64) -> u32 {
    u32::try_from(value).expect("bounded below a u32 maximum")
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
        "GUARDIAN_RS_TEST_RECHECK",
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
        assert_eq!(config.recheck_seconds, 60);
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
        assert_eq!(config.recheck_seconds, 30);
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
            // Past the bounds: values that would overflow a deadline.
            (VARS[1], "18446744073709551615"),
            (VARS[1], "2592001"),
            (VARS[2], "1001"),
            (VARS[3], "10001"),
            (VARS[4], "86401"),
            (VARS[5], "101"),
        ] {
            unsafe { std::env::set_var(var, bad) };
            assert!(from_test_vars().is_err(), "{var}={bad} must fail startup");
            unsafe { std::env::remove_var(var) };
        }
    }

    #[test]
    fn env_overrides_trim_whitespace_and_accept_the_bounds() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear_vars();
        unsafe {
            std::env::set_var(VARS[0], " true ");
            std::env::set_var(VARS[1], "2592000");
            std::env::set_var(VARS[2], " 1000");
            std::env::set_var(VARS[4], "86400\n");
        }
        let config = from_test_vars().expect("bounds are inclusive");
        assert!(config.enabled);
        assert_eq!(config.rotation_seconds, MAX_ROTATION_SECONDS);
        assert_eq!(config.max_rate_per_second, MAX_RATE_PER_SECOND);
        assert_eq!(config.recheck_seconds, MAX_RECHECK_SECONDS);
        clear_vars();
    }
}
