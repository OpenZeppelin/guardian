use std::time::Duration;

/// Environment override for [`CanonicalizationConfig::fast_promotion_enabled`].
pub const ENV_FAST_PROMOTION_ENABLED: &str = "GUARDIAN_CANONICALIZATION_FAST_PROMOTION_ENABLED";

/// Environment override for [`CanonicalizationConfig::max_concurrent_accounts`].
pub const ENV_MAX_CONCURRENT_ACCOUNTS: &str = "GUARDIAN_CANONICALIZATION_MAX_CONCURRENT_ACCOUNTS";

/// Environment override for [`CanonicalizationConfig::retained_ttl_seconds`].
/// `0` is the runtime kill switch: retention is disabled and the historical
/// delete-on-exhaustion behavior is restored without a recompile.
pub const ENV_RETAINED_TTL_SECONDS: &str = "GUARDIAN_CANONICALIZATION_RETAINED_TTL_SECONDS";

/// Environment override for [`CanonicalizationConfig::reconcile_interval_seconds`].
pub const ENV_RECONCILE_INTERVAL_SECONDS: &str =
    "GUARDIAN_CANONICALIZATION_RECONCILE_INTERVAL_SECONDS";

/// Configuration for delta canonicalization behavior
/// When Some: deltas are saved as candidates and later verified/canonicalized
/// When None: deltas are immediately saved as canonical (optimistic mode)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalizationConfig {
    /// How often the worker checks for deltas to canonicalize (in seconds)
    pub check_interval_seconds: u64,

    /// Whether recent candidates receive additional promotion-only checks.
    pub fast_promotion_enabled: bool,

    /// How often recent candidates receive an additional promotion-only check.
    pub fast_promotion_interval_seconds: u64,

    /// How long a candidate remains eligible for promotion-only checks.
    pub fast_promotion_window_seconds: u64,

    /// Maximum number of verification attempts before discarding the delta
    pub max_retries: u32,

    /// Minimum age a candidate must reach before verification failures consume retry budget.
    pub submission_grace_period_seconds: u64,

    /// Consecutive worker ticks that must observe the on-chain commitment at
    /// neither the candidate's previous nor its expected new commitment before
    /// the candidate is discarded as diverged (the account advanced past the
    /// state it was built on, so it can never verify). Values above 1 shield
    /// against acting on a single stale RPC read; the divergence discard
    /// bypasses the submission grace period.
    pub divergence_confirmations: u32,

    /// Minimum age a client abandon request (issue #319) must reach before
    /// the worker may finalize it. Together with
    /// `abandon_quarantine_checks` this quarantine reduces the risk of
    /// abandoning a transaction that lands late; like the divergence
    /// discard, abandon resolution bypasses the submission grace period.
    pub abandon_quarantine_seconds: u64,

    /// Consecutive worker ticks that must observe the on-chain commitment
    /// still at the candidate's base after an abandon request before the
    /// worker finalizes the abandon. A divergent observation resets the
    /// streak, mirroring `divergence_confirmations`.
    pub abandon_quarantine_checks: u32,

    /// How long a retry-exhausted candidate is kept as `retained`
    /// (issue #345) for background reconciliation before being dropped
    /// for good. A dedicated reconcile pass (see
    /// `reconcile_interval_seconds`) probes the chain for each account
    /// with recoverable rows and promotes a retained delta if the chain
    /// ever shows it landed — recovering an account whose stored state
    /// fell permanently behind after an RPC outage or worker downtime.
    /// `0` disables retention and restores the historical
    /// delete-on-exhaustion behavior.
    pub retained_ttl_seconds: u64,

    /// How often the dedicated reconcile pass over recoverable deltas
    /// runs (issue #345). Deliberately slower than
    /// `check_interval_seconds`: reconciliation is a background recovery
    /// sweep whose per-account cost starts with a chain RPC, and it must
    /// never crowd out ordinary candidate processing. Individual
    /// accounts are additionally backed off as their recoverable rows
    /// age (see the reconciliation module).
    pub reconcile_interval_seconds: u64,

    /// How many accounts one reconcile pass visits at most. Accounts
    /// beyond the page wait for the next pass; a rotation cursor keeps
    /// the selection fair, so a large backlog (e.g. after a correlated
    /// node outage) drains across passes instead of monopolizing one.
    pub reconcile_page_size: u32,
    /// How many accounts one canonicalization pass processes concurrently.
    /// Candidates within an account are always sequential (nonce order);
    /// this only overlaps the per-account work — dominated by the Miden
    /// RPC round trip — across accounts. `1` reproduces the fully
    /// sequential pass and is the safe rollback value. A DB connection is
    /// held only during the short fenced transactions, so this may exceed
    /// `GUARDIAN_DB_POOL_MAX_SIZE`; simultaneous write bursts queue
    /// briefly at the pool rather than failing.
    pub max_concurrent_accounts: usize,
}

impl Default for CanonicalizationConfig {
    fn default() -> Self {
        Self {
            check_interval_seconds: 10, // Try every 10 seconds
            fast_promotion_enabled: true,
            fast_promotion_interval_seconds: 3, // Follow Miden's block cadence
            fast_promotion_window_seconds: 30,
            max_retries: 18,                      // 18 attempts (total: ~3 minutes)
            submission_grace_period_seconds: 600, // Allow proving/submission to settle first
            divergence_confirmations: 2,          // Two ticks to rule out a stale read
            abandon_quarantine_seconds: 15,       // Let a late-landing tx surface first
            abandon_quarantine_checks: 2,         // Two ticks to rule out a stale read
            retained_ttl_seconds: 86_400,         // Reconcile a stuck base for up to a day (#345)
            reconcile_interval_seconds: 60,       // Recovery sweep; slower than the full pass
            reconcile_page_size: 100,             // Accounts per reconcile pass; cursor rotates
            max_concurrent_accounts: 10, // Overlaps per-account chain RPCs; prod Terraform sets 50
        }
    }
}

impl CanonicalizationConfig {
    /// Create a new canonicalization config with custom settings
    pub fn new(check_interval_seconds: u64, max_retries: u32) -> Self {
        Self {
            check_interval_seconds,
            max_retries,
            ..Self::default()
        }
    }

    /// Lease TTL the worker acquires per pass; outlives several renew cycles
    /// so a healthy holder never loses the lease mid-pass.
    pub fn lease_ttl(&self) -> Duration {
        self.check_interval() * 3
    }

    /// Get check interval as Duration
    pub fn check_interval(&self) -> Duration {
        Duration::from_secs(self.check_interval_seconds)
    }

    /// Get the recent-candidate promotion interval as a duration.
    pub fn fast_promotion_interval(&self) -> Duration {
        Duration::from_secs(self.fast_promotion_interval_seconds)
    }

    /// Override the cadence and eligibility window for promotion-only checks.
    pub fn with_fast_promotion(mut self, interval_seconds: u64, window_seconds: u64) -> Self {
        assert!(
            interval_seconds > 0,
            "fast promotion interval must be at least one second"
        );
        assert!(
            window_seconds > 0,
            "fast promotion window must be at least one second"
        );
        self.fast_promotion_interval_seconds = interval_seconds;
        self.fast_promotion_window_seconds = window_seconds;
        self
    }

    /// Enable or disable additional promotion-only checks for recent candidates.
    pub fn with_fast_promotion_enabled(mut self, enabled: bool) -> Self {
        self.fast_promotion_enabled = enabled;
        self
    }

    /// Apply the [`ENV_FAST_PROMOTION_ENABLED`] override when set.
    pub fn with_fast_promotion_enabled_from_env(self) -> Result<Self, String> {
        self.fast_promotion_enabled_from_var(ENV_FAST_PROMOTION_ENABLED)
    }

    fn fast_promotion_enabled_from_var(self, var_name: &str) -> Result<Self, String> {
        match std::env::var(var_name) {
            Ok(value) => value
                .parse::<bool>()
                .map(|enabled| self.with_fast_promotion_enabled(enabled))
                .map_err(|_| format!("{var_name} must be 'true' or 'false', got '{value}'")),
            Err(std::env::VarError::NotPresent) => Ok(self),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(format!("{var_name} contains invalid UTF-8"))
            }
        }
    }

    /// Override the submission grace period.
    pub fn with_submission_grace_period_seconds(mut self, seconds: u64) -> Self {
        self.submission_grace_period_seconds = seconds;
        self
    }

    /// Get submission grace period as Duration
    pub fn submission_grace_period(&self) -> Duration {
        Duration::from_secs(self.submission_grace_period_seconds)
    }

    /// Override the number of consecutive diverged observations required
    /// before a candidate is discarded.
    pub fn with_divergence_confirmations(mut self, confirmations: u32) -> Self {
        self.divergence_confirmations = confirmations;
        self
    }

    /// Override the abandon quarantine duration.
    pub fn with_abandon_quarantine_seconds(mut self, seconds: u64) -> Self {
        self.abandon_quarantine_seconds = seconds;
        self
    }

    /// Override the number of consecutive at-base observations required
    /// before an abandon request is finalized.
    pub fn with_abandon_quarantine_checks(mut self, checks: u32) -> Self {
        self.abandon_quarantine_checks = checks;
        self
    }

    /// Override how long retry-exhausted candidates are retained for
    /// background reconciliation. `0` disables retention (historical
    /// delete-on-exhaustion behavior).
    pub fn with_retained_ttl_seconds(mut self, seconds: u64) -> Self {
        self.retained_ttl_seconds = seconds;
        self
    }

    /// Apply the [`ENV_RETAINED_TTL_SECONDS`] override when set. `0` is
    /// the runtime kill switch for retention — no recompile needed to
    /// fall back to delete-on-exhaustion.
    pub fn with_retained_ttl_seconds_from_env(self) -> Result<Self, String> {
        self.retained_ttl_seconds_from_var(ENV_RETAINED_TTL_SECONDS)
    }

    fn retained_ttl_seconds_from_var(self, var_name: &str) -> Result<Self, String> {
        match std::env::var(var_name) {
            Ok(value) => value
                .parse::<u64>()
                .map(|seconds| self.with_retained_ttl_seconds(seconds))
                .map_err(|_| format!("{var_name} must be a non-negative integer, got '{value}'")),
            Err(std::env::VarError::NotPresent) => Ok(self),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(format!("{var_name} contains invalid UTF-8"))
            }
        }
    }

    /// Get the reconcile pass interval as a duration.
    pub fn reconcile_interval(&self) -> Duration {
        Duration::from_secs(self.reconcile_interval_seconds)
    }

    /// Override how often the reconcile pass over recoverable deltas runs.
    pub fn with_reconcile_interval_seconds(mut self, seconds: u64) -> Self {
        assert!(
            seconds > 0,
            "reconcile interval must be at least one second"
        );
        self.reconcile_interval_seconds = seconds;
        self
    }

    /// Apply the [`ENV_RECONCILE_INTERVAL_SECONDS`] override when set.
    pub fn with_reconcile_interval_seconds_from_env(self) -> Result<Self, String> {
        self.reconcile_interval_seconds_from_var(ENV_RECONCILE_INTERVAL_SECONDS)
    }

    fn reconcile_interval_seconds_from_var(self, var_name: &str) -> Result<Self, String> {
        match std::env::var(var_name) {
            Ok(value) => {
                let seconds = value
                    .parse::<u64>()
                    .map_err(|_| format!("{var_name} must be a positive integer, got '{value}'"))?;
                if seconds == 0 {
                    return Err(format!("{var_name} must be greater than zero"));
                }
                Ok(self.with_reconcile_interval_seconds(seconds))
            }
            Err(std::env::VarError::NotPresent) => Ok(self),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(format!("{var_name} contains invalid UTF-8"))
            }
        }
    }

    /// Override how many accounts one reconcile pass visits at most.
    pub fn with_reconcile_page_size(mut self, accounts: u32) -> Self {
        assert!(
            accounts > 0,
            "reconcile page size must be at least one account"
        );
        self.reconcile_page_size = accounts;
        self
    }

    /// Override how many accounts one pass processes concurrently.
    /// `1` reproduces the fully sequential pass.
    pub fn with_max_concurrent_accounts(mut self, accounts: usize) -> Self {
        assert!(
            accounts > 0,
            "max_concurrent_accounts must be at least 1 (1 = fully sequential)"
        );
        self.max_concurrent_accounts = accounts;
        self
    }

    /// Apply the [`ENV_MAX_CONCURRENT_ACCOUNTS`] override when set. An
    /// unset variable keeps the built-in default; a present-but-invalid
    /// value fails startup loudly — a silently ignored typo here would
    /// run production at the wrong concurrency.
    pub fn with_max_concurrent_accounts_from_env(self) -> Result<Self, String> {
        self.max_concurrent_accounts_from_var(ENV_MAX_CONCURRENT_ACCOUNTS)
    }

    fn max_concurrent_accounts_from_var(self, var_name: &str) -> Result<Self, String> {
        match std::env::var(var_name) {
            Ok(value) => {
                let accounts = value
                    .parse::<usize>()
                    .map_err(|_| format!("{var_name} must be a positive integer, got '{value}'"))?;
                if accounts == 0 {
                    return Err(format!("{var_name} must be greater than zero"));
                }
                Ok(self.with_max_concurrent_accounts(accounts))
            }
            Err(std::env::VarError::NotPresent) => Ok(self),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(format!("{var_name} contains invalid UTF-8"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::env_lock::ENV_LOCK;

    #[test]
    fn default_fast_promotion_is_bounded() {
        let config = CanonicalizationConfig::default();

        assert!(config.fast_promotion_enabled);
        assert_eq!(config.fast_promotion_interval_seconds, 3);
        assert_eq!(config.fast_promotion_window_seconds, 30);
    }

    #[test]
    fn fast_promotion_builder_overrides_cadence_and_window() {
        let config = CanonicalizationConfig::default().with_fast_promotion(3, 45);

        assert_eq!(config.fast_promotion_interval_seconds, 3);
        assert_eq!(config.fast_promotion_window_seconds, 45);
    }

    #[test]
    fn fast_promotion_enabled_env_override_is_strict() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let var_name = "GUARDIAN_CANON_FAST_PROMOTION_TEST";

        unsafe { std::env::set_var(var_name, "false") };
        let config = CanonicalizationConfig::default()
            .fast_promotion_enabled_from_var(var_name)
            .expect("valid boolean applies");
        assert!(!config.fast_promotion_enabled);

        unsafe { std::env::set_var(var_name, "0") };
        assert!(
            CanonicalizationConfig::default()
                .fast_promotion_enabled_from_var(var_name)
                .is_err()
        );

        unsafe { std::env::remove_var(var_name) };
    }

    #[test]
    fn env_override_missing_keeps_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let var_name = "GUARDIAN_CANON_CONCURRENCY_TEST_MISSING";
        unsafe { std::env::remove_var(var_name) };

        let config = CanonicalizationConfig::default()
            .max_concurrent_accounts_from_var(var_name)
            .expect("missing variable is not an error");

        assert_eq!(config.max_concurrent_accounts, 10);
    }

    #[test]
    fn env_override_applies_parsed_value() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let var_name = "GUARDIAN_CANON_CONCURRENCY_TEST_PRESENT";
        unsafe { std::env::set_var(var_name, "24") };

        let config = CanonicalizationConfig::default()
            .max_concurrent_accounts_from_var(var_name)
            .expect("valid value applies");

        assert_eq!(config.max_concurrent_accounts, 24);
        unsafe { std::env::remove_var(var_name) };
    }

    #[test]
    fn retained_ttl_env_override_accepts_zero_kill_switch() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let var_name = "GUARDIAN_CANON_RETAINED_TTL_TEST";

        unsafe { std::env::set_var(var_name, "0") };
        let config = CanonicalizationConfig::default()
            .retained_ttl_seconds_from_var(var_name)
            .expect("zero is the documented kill switch");
        assert_eq!(config.retained_ttl_seconds, 0);

        unsafe { std::env::set_var(var_name, "3600") };
        let config = CanonicalizationConfig::default()
            .retained_ttl_seconds_from_var(var_name)
            .expect("valid value applies");
        assert_eq!(config.retained_ttl_seconds, 3600);

        unsafe { std::env::set_var(var_name, "not-a-number") };
        assert!(
            CanonicalizationConfig::default()
                .retained_ttl_seconds_from_var(var_name)
                .is_err()
        );

        unsafe { std::env::remove_var(var_name) };
    }

    #[test]
    fn reconcile_interval_env_override_rejects_zero() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let var_name = "GUARDIAN_CANON_RECONCILE_INTERVAL_TEST";

        unsafe { std::env::set_var(var_name, "120") };
        let config = CanonicalizationConfig::default()
            .reconcile_interval_seconds_from_var(var_name)
            .expect("valid value applies");
        assert_eq!(config.reconcile_interval_seconds, 120);

        unsafe { std::env::set_var(var_name, "0") };
        assert!(
            CanonicalizationConfig::default()
                .reconcile_interval_seconds_from_var(var_name)
                .is_err(),
            "a zero interval would spin the reconcile timer"
        );

        unsafe { std::env::remove_var(var_name) };
        let config = CanonicalizationConfig::default()
            .reconcile_interval_seconds_from_var(var_name)
            .expect("missing variable is not an error");
        assert_eq!(config.reconcile_interval_seconds, 60);
    }

    #[test]
    fn env_override_rejects_zero_and_garbage() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let var_name = "GUARDIAN_CANON_CONCURRENCY_TEST_INVALID";

        unsafe { std::env::set_var(var_name, "0") };
        assert!(
            CanonicalizationConfig::default()
                .max_concurrent_accounts_from_var(var_name)
                .is_err()
        );

        unsafe { std::env::set_var(var_name, "not-a-number") };
        assert!(
            CanonicalizationConfig::default()
                .max_concurrent_accounts_from_var(var_name)
                .is_err()
        );

        unsafe { std::env::remove_var(var_name) };
    }
}
