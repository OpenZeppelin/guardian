//! Request body size limit configuration
//!
//! Configures the maximum allowed request body size for HTTP endpoints.

use std::env;

/// Default max request body size: 1 MB
const DEFAULT_MAX_REQUEST_BYTES: usize = 1024 * 1024;

/// Environment variable name for max request bytes
const ENV_VAR_NAME: &str = "GUARDIAN_MAX_REQUEST_BYTES";

/// Body size limit configuration
#[derive(Debug, Clone, Copy)]
pub struct BodyLimitConfig {
    /// Maximum request body size in bytes
    pub max_bytes: usize,
}

impl BodyLimitConfig {
    /// Load configuration from environment variables
    ///
    /// Reads `GUARDIAN_MAX_REQUEST_BYTES` from environment.
    /// Defaults to 1 MB if not set or invalid.
    pub fn from_env() -> Self {
        Self::from_env_var(ENV_VAR_NAME)
    }

    /// Load configuration from a specific environment variable (for testing)
    fn from_env_var(var_name: &str) -> Self {
        let max_bytes = env::var(var_name)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MAX_REQUEST_BYTES);

        Self { max_bytes }
    }

    /// Create a new config with custom max bytes
    pub fn new(max_bytes: usize) -> Self {
        Self { max_bytes }
    }
}

impl Default for BodyLimitConfig {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_REQUEST_BYTES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = BodyLimitConfig::default();
        assert_eq!(config.max_bytes, 1024 * 1024); // 1 MB
    }

    #[test]
    fn test_new_config() {
        let config = BodyLimitConfig::new(5 * 1024 * 1024);
        assert_eq!(config.max_bytes, 5 * 1024 * 1024);
    }

    #[test]
    fn test_from_env_var_not_set() {
        // Use a unique env var name that won't conflict with other tests
        let config = BodyLimitConfig::from_env_var("GUARDIAN_TEST_BODY_LIMIT_UNSET_12345");
        assert_eq!(config.max_bytes, 1024 * 1024);
    }

    #[test]
    fn test_from_env_var_custom() {
        let var_name = "GUARDIAN_TEST_BODY_LIMIT_CUSTOM_67890";
        unsafe {
            env::set_var(var_name, "2097152");
        }
        let config = BodyLimitConfig::from_env_var(var_name);
        assert_eq!(config.max_bytes, 2097152); // 2 MB
        unsafe {
            env::remove_var(var_name);
        }
    }

    #[test]
    fn test_from_env_var_invalid() {
        let var_name = "GUARDIAN_TEST_BODY_LIMIT_INVALID_11111";
        unsafe {
            env::set_var(var_name, "not_a_number");
        }
        let config = BodyLimitConfig::from_env_var(var_name);
        assert_eq!(config.max_bytes, 1024 * 1024); // Falls back to default
        unsafe {
            env::remove_var(var_name);
        }
    }

    #[test]
    fn test_from_env_var_zero() {
        let var_name = "GUARDIAN_TEST_BODY_LIMIT_ZERO_22222";
        unsafe {
            env::set_var(var_name, "0");
        }
        let config = BodyLimitConfig::from_env_var(var_name);
        assert_eq!(config.max_bytes, 0);
        unsafe {
            env::remove_var(var_name);
        }
    }

    #[test]
    fn test_from_env_var_large_value() {
        let var_name = "GUARDIAN_TEST_BODY_LIMIT_LARGE_33333";
        unsafe {
            env::set_var(var_name, "104857600"); // 100 MB
        }
        let config = BodyLimitConfig::from_env_var(var_name);
        assert_eq!(config.max_bytes, 104857600);
        unsafe {
            env::remove_var(var_name);
        }
    }
}
