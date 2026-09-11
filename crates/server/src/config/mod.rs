pub mod stage;

/// Reads an optional positive-integer environment variable, rejecting zero
/// and malformed values with the variable name in the error.
pub(crate) fn positive_u32_from_env(key: &str, default: u32) -> Result<u32, String> {
    match std::env::var(key) {
        Ok(raw) => match raw.trim().parse::<u32>() {
            Ok(0) => Err(format!("{key} must be a positive integer, got 0")),
            Ok(value) => Ok(value),
            Err(_) => Err(format!("{key} must be a positive integer, got {raw:?}")),
        },
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{key} must contain valid UTF-8")),
    }
}
