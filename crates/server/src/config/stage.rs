use crate::error::{GuardianError, Result};

const ENV_GUARDIAN_ENV: &str = "GUARDIAN_ENV";
const PROD_ENV: &str = "prod";

/// True when the deployment stage is production (`GUARDIAN_ENV=prod`,
/// case-insensitive). Gates production-only startup guards.
pub fn is_prod() -> Result<bool> {
    match std::env::var(ENV_GUARDIAN_ENV) {
        Ok(value) => Ok(value.eq_ignore_ascii_case(PROD_ENV)),
        Err(std::env::VarError::NotPresent) => Ok(false),
        Err(std::env::VarError::NotUnicode(_)) => Err(GuardianError::ConfigurationError(format!(
            "{ENV_GUARDIAN_ENV} must contain valid UTF-8"
        ))),
    }
}
