//! The rate limit on dry-run proposal checks, per account and signer. Process-wide and
//! independent of the module catalog: a check rebuilds and verifies the account's state even
//! when no module is configured.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::error::GuardianError;

/// Env var for how many dry-run checks one signer may make for one account per minute.
pub const CHECK_RATE_ENV: &str = "GUARDIAN_MODULE_CHECK_RATE_PER_MIN";
const CHECK_RATE_DEFAULT: usize = 30;
const CHECK_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// Per account and signer sliding window for dry-run checks, so the check cannot be used to
/// enumerate a denylist or a model's thresholds. Per replica.
pub struct CheckLimiter {
    per_minute: usize,
    calls:
        std::sync::Mutex<HashMap<(String, String), std::collections::VecDeque<std::time::Instant>>>,
}

fn check_rate(raw: Result<String, std::env::VarError>) -> Result<usize, String> {
    match raw {
        Err(std::env::VarError::NotPresent) => Ok(CHECK_RATE_DEFAULT),
        Ok(value) => value
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|limit| *limit > 0)
            .ok_or_else(|| format!("{CHECK_RATE_ENV} must be a positive integer, got {value:?}")),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(format!("{CHECK_RATE_ENV} must contain valid UTF-8"))
        }
    }
}

impl CheckLimiter {
    pub fn with_limit(per_minute: usize) -> Self {
        Self {
            per_minute,
            calls: Default::default(),
        }
    }

    /// The limit from `GUARDIAN_MODULE_CHECK_RATE_PER_MIN`, the default when unset; a value
    /// that is not a positive integer refuses startup.
    fn from_env() -> Result<Self, String> {
        check_rate(std::env::var(CHECK_RATE_ENV)).map(Self::with_limit)
    }

    pub fn admit(&self, account_id: &str, signer: &str) -> Result<(), GuardianError> {
        let now = std::time::Instant::now();
        let mut calls = self
            .calls
            .lock()
            .map_err(|_| GuardianError::StorageError("check limiter poisoned".into()))?;
        calls.retain(|_, window| {
            window.retain(|at| now.duration_since(*at) < CHECK_WINDOW);
            !window.is_empty()
        });
        let window = calls
            .entry((account_id.to_ascii_lowercase(), signer.to_ascii_lowercase()))
            .or_default();
        if window.len() >= self.per_minute {
            let retry_after = window
                .front()
                .map(|oldest| CHECK_WINDOW.saturating_sub(now.duration_since(*oldest)))
                .unwrap_or(CHECK_WINDOW);
            return Err(GuardianError::RateLimitExceeded {
                retry_after_secs: u32::try_from(retry_after.as_secs().max(1)).unwrap_or(60),
                scope: "proposal_check".into(),
            });
        }
        window.push_back(now);
        Ok(())
    }
}

static LIMITER: OnceLock<CheckLimiter> = OnceLock::new();

/// Installs the limiter from `GUARDIAN_MODULE_CHECK_RATE_PER_MIN`; a malformed value fails
/// startup. Called once by the server builder.
pub fn init_from_env() -> Result<(), String> {
    let limiter = CheckLimiter::from_env()?;
    let _ = LIMITER.set(limiter);
    Ok(())
}

/// Counts one check by `signer` for `account_id`, or refuses it with `rate_limit_exceeded`.
pub fn admit(account_id: &str, signer: &str) -> Result<(), GuardianError> {
    LIMITER
        .get_or_init(|| CheckLimiter::with_limit(CHECK_RATE_DEFAULT))
        .admit(account_id, signer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_rate_is_a_positive_integer_or_startup_fails() {
        use std::env::VarError;
        assert_eq!(
            check_rate(Err(VarError::NotPresent)),
            Ok(CHECK_RATE_DEFAULT)
        );
        assert_eq!(check_rate(Ok(" 12 ".into())), Ok(12));
        for bad in ["0", "-3", "thirty", "1.5", ""] {
            assert!(check_rate(Ok(bad.into())).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn dry_runs_are_limited_per_account_and_signer() {
        let limiter = CheckLimiter::with_limit(3);
        for _ in 0..3 {
            limiter.admit("0xacc", "0xsigner").unwrap();
        }
        assert!(matches!(
            limiter.admit("0xacc", "0xsigner"),
            Err(GuardianError::RateLimitExceeded { .. })
        ));
        assert!(limiter.admit("0xacc", "0xother").is_ok());
        assert!(limiter.admit("0xother", "0xsigner").is_ok());
    }
}
