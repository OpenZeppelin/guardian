use std::time::Duration;

/// Environment override for [`CanonicalizationConfig::max_concurrent_accounts`].
/// The other canonicalization knobs remain code-configured.
pub const ENV_MAX_CONCURRENT_ACCOUNTS: &str = "GUARDIAN_CANONICALIZATION_MAX_CONCURRENT_ACCOUNTS";

/// Configuration for delta canonicalization behavior
/// When Some: deltas are saved as candidates and later verified/canonicalized
/// When None: deltas are immediately saved as canonical (optimistic mode)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalizationConfig {
    /// How often the worker checks for deltas to canonicalize (in seconds)
    pub check_interval_seconds: u64,

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
            check_interval_seconds: 10,           // Try every 10 seconds
            max_retries: 18,                      // 18 attempts (total: ~3 minutes)
            submission_grace_period_seconds: 600, // Allow proving/submission to settle first
            divergence_confirmations: 2,          // Two ticks to rule out a stale read
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

    /// Get check interval as Duration
    pub fn check_interval(&self) -> Duration {
        Duration::from_secs(self.check_interval_seconds)
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

    /// Override how many accounts one pass processes concurrently.
    /// `1` reproduces the fully sequential pass.
    pub fn with_max_concurrent_accounts(mut self, accounts: usize) -> Self {
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

    #[test]
    fn env_override_missing_keeps_default() {
        let var_name = "GUARDIAN_CANON_CONCURRENCY_TEST_MISSING";
        unsafe { std::env::remove_var(var_name) };

        let config = CanonicalizationConfig::default()
            .max_concurrent_accounts_from_var(var_name)
            .expect("missing variable is not an error");

        assert_eq!(config.max_concurrent_accounts, 10);
    }

    #[test]
    fn env_override_applies_parsed_value() {
        let var_name = "GUARDIAN_CANON_CONCURRENCY_TEST_PRESENT";
        unsafe { std::env::set_var(var_name, "24") };

        let config = CanonicalizationConfig::default()
            .max_concurrent_accounts_from_var(var_name)
            .expect("valid value applies");

        assert_eq!(config.max_concurrent_accounts, 24);
        unsafe { std::env::remove_var(var_name) };
    }

    #[test]
    fn env_override_rejects_zero_and_garbage() {
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
