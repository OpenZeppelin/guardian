//! The rate limits on learning what policy modules refuse, per account and signer: dry-run
//! proposal checks, and proposals the modules deny at creation (which store nothing, so without
//! a limit creation would be a free check). Process-wide and independent of the module catalog:
//! a check rebuilds and verifies the account's state even when no module is configured.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::error::GuardianError;

/// Env var for how many dry-run checks one signer may make for one account per minute, and how
/// many of their proposals the modules may deny at creation per minute before creation is
/// refused for the rest of the window. The two are counted separately.
pub const CHECK_RATE_ENV: &str = "GUARDIAN_MODULE_CHECK_RATE_PER_MIN";
pub(crate) const CHECK_RATE_DEFAULT: usize = 30;
const CHECK_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// Per account and signer sliding window, so neither the check nor creation can be used to
/// enumerate a denylist or a model's thresholds. Per replica.
pub struct CheckLimiter {
    per_minute: usize,
    /// The `scope` of the `rate_limit_exceeded` refusal.
    scope: &'static str,
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
    pub fn with_limit(per_minute: usize, scope: &'static str) -> Self {
        Self {
            per_minute,
            scope,
            calls: Default::default(),
        }
    }

    /// Counts one call, or refuses it with `rate_limit_exceeded` when the window is full.
    /// Returns the entry it counted, which [`Self::release`] takes back.
    pub fn admit(
        &self,
        account_id: &str,
        signer: &str,
    ) -> Result<std::time::Instant, GuardianError> {
        let mut counted = None;
        self.with_window(account_id, signer, |window, now| {
            self.refuse_when_full(window, now)?;
            window.push_back(now);
            counted = Some(now);
            Ok(())
        })?;
        counted.ok_or_else(|| GuardianError::StorageError("check limiter counted nothing".into()))
    }

    /// Takes back an entry [`Self::admit`] counted, when the call it reserved for turned out
    /// not to count.
    pub fn release(&self, account_id: &str, signer: &str, counted: std::time::Instant) {
        let _ = self.with_window(account_id, signer, |window, _| {
            if let Some(position) = window.iter().position(|at| *at == counted) {
                window.remove(position);
            }
            Ok(())
        });
    }

    /// Refuses with `rate_limit_exceeded` when the window is full, counting nothing.
    fn refuse_when_full(
        &self,
        window: &std::collections::VecDeque<std::time::Instant>,
        now: std::time::Instant,
    ) -> Result<(), GuardianError> {
        if window.len() < self.per_minute {
            return Ok(());
        }
        let retry_after = window
            .front()
            .map(|oldest| CHECK_WINDOW.saturating_sub(now.duration_since(*oldest)))
            .unwrap_or(CHECK_WINDOW);
        Err(GuardianError::RateLimitExceeded {
            retry_after_secs: u32::try_from(retry_after.as_secs().max(1)).unwrap_or(60),
            scope: self.scope.into(),
        })
    }

    fn with_window(
        &self,
        account_id: &str,
        signer: &str,
        apply: impl FnOnce(
            &mut std::collections::VecDeque<std::time::Instant>,
            std::time::Instant,
        ) -> Result<(), GuardianError>,
    ) -> Result<(), GuardianError> {
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
        apply(window, now)
    }
}

static CHECKS: OnceLock<CheckLimiter> = OnceLock::new();
static DENIALS: OnceLock<CheckLimiter> = OnceLock::new();
const CHECK_SCOPE: &str = "proposal_check";
const DENIAL_SCOPE: &str = "proposal_denials";

/// Installs the limiters from `GUARDIAN_MODULE_CHECK_RATE_PER_MIN`; a malformed value fails
/// startup. Called once by the server builder.
pub fn init_from_env() -> Result<(), String> {
    let per_minute = check_rate(std::env::var(CHECK_RATE_ENV))?;
    let _ = CHECKS.set(CheckLimiter::with_limit(per_minute, CHECK_SCOPE));
    let _ = DENIALS.set(CheckLimiter::with_limit(per_minute, DENIAL_SCOPE));
    Ok(())
}

fn checks() -> &'static CheckLimiter {
    CHECKS.get_or_init(|| CheckLimiter::with_limit(CHECK_RATE_DEFAULT, CHECK_SCOPE))
}

fn denials() -> &'static CheckLimiter {
    DENIALS.get_or_init(|| CheckLimiter::with_limit(CHECK_RATE_DEFAULT, DENIAL_SCOPE))
}

/// Counts one check by `signer` for `account_id`, or refuses it with `rate_limit_exceeded`.
pub fn admit(account_id: &str, signer: &str) -> Result<(), GuardianError> {
    checks().admit(account_id, signer).map(drop)
}

/// Reserves a place in `signer`'s window of denied creations for `account_id` before the
/// modules run, or refuses the creation with `rate_limit_exceeded` once the window is full.
/// The reservation counts only if the modules deny the proposal ([`CreationSlot::denied`]);
/// dropped otherwise, it is given back. Reserving before the modules run keeps creations in
/// flight at once from all being judged against the same not yet full window.
pub fn admit_creation(account_id: &str, signer: &str) -> Result<CreationSlot, GuardianError> {
    denials()
        .admit(account_id, signer)
        .map(|counted| CreationSlot {
            account_id: account_id.to_string(),
            signer: signer.to_string(),
            counted,
            denied: false,
        })
}

/// How many of `signer`'s creations for `account_id` the window counts now.
#[cfg(test)]
pub(crate) fn denials_counted(account_id: &str, signer: &str) -> usize {
    let mut counted = 0;
    let _ = denials().with_window(account_id, signer, |window, _| {
        counted = window.len();
        Ok(())
    });
    counted
}

/// A place reserved in a window of denied creations, from [`admit_creation`].
#[must_use = "a reservation is given back unless the creation is denied"]
pub struct CreationSlot {
    account_id: String,
    signer: String,
    counted: std::time::Instant,
    denied: bool,
}

impl CreationSlot {
    /// The modules denied the proposal: the reservation stays counted.
    pub fn denied(mut self) {
        self.denied = true;
    }
}

impl Drop for CreationSlot {
    fn drop(&mut self) {
        if !self.denied {
            denials().release(&self.account_id, &self.signer, self.counted);
        }
    }
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
        let limiter = CheckLimiter::with_limit(3, CHECK_SCOPE);
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

    #[test]
    fn a_reservation_counts_only_when_kept() {
        let limiter = CheckLimiter::with_limit(2, DENIAL_SCOPE);
        let given_back = limiter.admit("0xacc", "0xsigner").unwrap();
        limiter.release("0xacc", "0xsigner", given_back);
        limiter.admit("0xacc", "0xsigner").unwrap();
        limiter.admit("0xacc", "0xsigner").unwrap();
        assert!(matches!(
            limiter.admit("0xacc", "0xsigner"),
            Err(GuardianError::RateLimitExceeded { ref scope, .. }) if scope == DENIAL_SCOPE
        ));
        assert!(
            limiter.admit("0xacc", "0xother").is_ok(),
            "another signer's denials are counted apart"
        );
    }
}
