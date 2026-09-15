use crate::builder::logging::LogFormat;
use crate::error::{GuardianError, Result};

const ENV_GUARDIAN_ENV: &str = "GUARDIAN_ENV";
const PROD_ENV: &str = "prod";

/// Deployment stage selected by `GUARDIAN_ENV`.
///
/// `Prod` turns on the production startup guards and supplies production
/// defaults for the runtime knobs that are otherwise sized for local
/// development. The defaults mirror what the AWS Terraform `prod` profile
/// injects, so a self-managed deployment that sets `GUARDIAN_ENV=prod` starts
/// with the same values without listing each variable. An explicitly set
/// variable always wins over the stage default.
///
/// Readers with fallible constructors (storage, canonicalization) propagate a
/// non-UTF-8 `GUARDIAN_ENV` as a startup error; the infallible ones (rate
/// limit, log format) warn and use the `Dev` defaults. The server binary
/// resolves the stage with [`Stage::from_env`] and aborts on an invalid value
/// before it constructs the infallible readers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Dev,
    Prod,
}

impl Stage {
    /// Resolves the stage from `GUARDIAN_ENV` (`prod`, case-insensitive, is
    /// production; anything else or unset is development).
    pub fn from_env() -> Result<Self> {
        match std::env::var(ENV_GUARDIAN_ENV) {
            Ok(value) if value.trim().eq_ignore_ascii_case(PROD_ENV) => Ok(Self::Prod),
            Ok(_) | Err(std::env::VarError::NotPresent) => Ok(Self::Dev),
            Err(std::env::VarError::NotUnicode(_)) => Err(GuardianError::ConfigurationError(
                format!("{ENV_GUARDIAN_ENV} must contain valid UTF-8"),
            )),
        }
    }

    pub fn is_prod(self) -> bool {
        matches!(self, Self::Prod)
    }

    /// Default for `GUARDIAN_RATE_BURST_PER_SEC` (per IP and endpoint).
    pub fn default_rate_burst_per_sec(self) -> u32 {
        match self {
            Self::Dev => 10,
            Self::Prod => 200,
        }
    }

    /// Default for `GUARDIAN_RATE_PER_MIN` (per IP across HTTP and gRPC).
    pub fn default_rate_per_min(self) -> u32 {
        match self {
            Self::Dev => 60,
            Self::Prod => 5000,
        }
    }

    /// Default for `GUARDIAN_DB_POOL_MAX_SIZE`; the metadata pool follows it.
    pub fn default_db_pool_max_size(self) -> usize {
        match self {
            Self::Dev => 16,
            Self::Prod => 32,
        }
    }

    /// Default for `GUARDIAN_CANONICALIZATION_MAX_CONCURRENT_ACCOUNTS`.
    pub fn default_canonicalization_max_concurrent_accounts(self) -> usize {
        match self {
            Self::Dev => 10,
            Self::Prod => 50,
        }
    }

    /// Default for `GUARDIAN_LOG_FORMAT`.
    pub fn default_log_format(self) -> LogFormat {
        match self {
            Self::Dev => LogFormat::Text,
            Self::Prod => LogFormat::Json,
        }
    }
}

/// True when the deployment stage is production (`GUARDIAN_ENV=prod`,
/// case-insensitive). Gates production-only startup guards.
pub fn is_prod() -> Result<bool> {
    Ok(Stage::from_env()?.is_prod())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::env_lock::ENV_LOCK;

    fn with_guardian_env<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        // SAFETY: serialized by ENV_LOCK; the variable is restored before returning.
        unsafe {
            match value {
                Some(value) => std::env::set_var(ENV_GUARDIAN_ENV, value),
                None => std::env::remove_var(ENV_GUARDIAN_ENV),
            }
        }
        let result = f();
        // SAFETY: serialized by ENV_LOCK.
        unsafe { std::env::remove_var(ENV_GUARDIAN_ENV) };
        result
    }

    #[test]
    fn unset_and_non_prod_values_are_dev() {
        assert_eq!(
            with_guardian_env(None, Stage::from_env).unwrap(),
            Stage::Dev
        );
        assert_eq!(
            with_guardian_env(Some("staging"), Stage::from_env).unwrap(),
            Stage::Dev
        );
    }

    #[test]
    fn prod_is_case_insensitive_and_trimmed() {
        assert_eq!(
            with_guardian_env(Some(" PROD "), Stage::from_env).unwrap(),
            Stage::Prod
        );
        assert!(with_guardian_env(Some("prod"), is_prod).unwrap());
    }

    #[test]
    fn prod_defaults_match_the_aws_prod_profile() {
        assert_eq!(Stage::Prod.default_rate_burst_per_sec(), 200);
        assert_eq!(Stage::Prod.default_rate_per_min(), 5000);
        assert_eq!(Stage::Prod.default_db_pool_max_size(), 32);
        assert_eq!(
            Stage::Prod.default_canonicalization_max_concurrent_accounts(),
            50
        );
        assert_eq!(Stage::Prod.default_log_format(), LogFormat::Json);
    }

    #[test]
    fn dev_defaults_are_sized_for_local_development() {
        assert_eq!(Stage::Dev.default_rate_burst_per_sec(), 10);
        assert_eq!(Stage::Dev.default_rate_per_min(), 60);
        assert_eq!(Stage::Dev.default_db_pool_max_size(), 16);
        assert_eq!(
            Stage::Dev.default_canonicalization_max_concurrent_accounts(),
            10
        );
        assert_eq!(Stage::Dev.default_log_format(), LogFormat::Text);
    }
}
