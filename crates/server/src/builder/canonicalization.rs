use std::time::Duration;

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
}

impl Default for CanonicalizationConfig {
    fn default() -> Self {
        Self {
            check_interval_seconds: 10,           // Try every 10 seconds
            max_retries: 18,                      // 18 attempts (total: ~3 minutes)
            submission_grace_period_seconds: 600, // Allow proving/submission to settle first
            divergence_confirmations: 2,          // Two ticks to rule out a stale read
        }
    }
}

impl CanonicalizationConfig {
    /// Create a new canonicalization config with custom settings
    pub fn new(check_interval_seconds: u64, max_retries: u32) -> Self {
        Self {
            check_interval_seconds,
            max_retries,
            submission_grace_period_seconds: Self::default().submission_grace_period_seconds,
            divergence_confirmations: Self::default().divergence_confirmations,
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
}
