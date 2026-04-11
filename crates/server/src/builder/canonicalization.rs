use std::time::Duration;

/// Configuration for delta canonicalization behavior
/// When Some: deltas are saved as candidates and later verified/canonicalized
/// When None: deltas are immediately saved as canonical (optimistic mode)
#[derive(Clone, Debug)]
pub struct CanonicalizationConfig {
    /// How often the worker checks for deltas to canonicalize (in seconds)
    pub check_interval_seconds: u64,

    /// Maximum number of verification attempts before discarding the delta
    pub max_retries: u32,

    /// Minimum age a candidate must reach before verification failures consume retry budget.
    pub submission_grace_period_seconds: u64,
}

impl Default for CanonicalizationConfig {
    fn default() -> Self {
        Self {
            check_interval_seconds: 10,           // Try every 10 seconds
            max_retries: 18,                      // 18 attempts (total: ~3 minutes)
            submission_grace_period_seconds: 600, // Allow proving/submission to settle first
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
}
