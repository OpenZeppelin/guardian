//! Bounds on policy module evaluation: the per-invocation time budget, the request size cap, and
//! the circuit breaker and health probe settings.

use std::time::Duration;

pub const BUDGET_ENV: &str = "GUARDIAN_MODULE_EVALUATION_BUDGET_MS";
pub const REQUEST_MAX_BYTES_ENV: &str = "GUARDIAN_MODULE_REQUEST_MAX_BYTES";
pub const BREAKER_FAILURES_ENV: &str = "GUARDIAN_MODULE_BREAKER_FAILURES";
pub const PROBE_INTERVAL_ENV: &str = "GUARDIAN_MODULE_PROBE_INTERVAL_MS";
pub const PARAMS_MAX_BYTES_ENV: &str = "GUARDIAN_MODULE_PARAMS_MAX_BYTES";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Time spent in module calls for one hook invocation; request assembly is not charged.
    pub budget: Duration,
    /// Largest encoded request sent to a module; a larger one is a module failure, not sent.
    pub request_max_bytes: usize,
    /// Consecutive failures that open an instance's breaker.
    pub breaker_failures: u32,
    /// Interval of the `Describe` health probe per instance.
    pub probe_interval: Duration,
    /// Largest params value one write may store, as compact JSON with sorted keys.
    pub params_max_bytes: usize,
}

impl Limits {
    /// Refuses `params` larger than [`Self::params_max_bytes`].
    pub fn check_params_size(&self, params: &serde_json::Value) -> Result<(), String> {
        let size = serde_json::to_vec(params).map_or(usize::MAX, |bytes| bytes.len());
        if size > self.params_max_bytes {
            return Err(format!(
                "params of {size} bytes exceed {PARAMS_MAX_BYTES_ENV} ({} bytes)",
                self.params_max_bytes
            ));
        }
        Ok(())
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            budget: Duration::from_millis(1_000),
            request_max_bytes: guardian_module_sdk::DEFAULT_REQUEST_MAX_BYTES,
            breaker_failures: 5,
            probe_interval: Duration::from_millis(5_000),
            params_max_bytes: 64 * 1024,
        }
    }
}

impl Limits {
    /// The limits from the environment, each defaulting when unset; a value that is not a
    /// positive integer refuses startup.
    pub fn from_env() -> Result<Self, String> {
        Self::from_vars(|name| std::env::var(name))
    }

    fn from_vars(var: impl Fn(&str) -> Result<String, std::env::VarError>) -> Result<Self, String> {
        let defaults = Self::default();
        Ok(Self {
            budget: Duration::from_millis(
                positive(&var, BUDGET_ENV)?
                    .unwrap_or(u64::try_from(defaults.budget.as_millis()).unwrap_or(u64::MAX)),
            ),
            request_max_bytes: positive(&var, REQUEST_MAX_BYTES_ENV)?
                .map(|bytes| usize::try_from(bytes).unwrap_or(usize::MAX))
                .unwrap_or(defaults.request_max_bytes),
            breaker_failures: positive(&var, BREAKER_FAILURES_ENV)?
                .map(|failures| u32::try_from(failures).unwrap_or(u32::MAX))
                .unwrap_or(defaults.breaker_failures),
            probe_interval: positive(&var, PROBE_INTERVAL_ENV)?
                .map(Duration::from_millis)
                .unwrap_or(defaults.probe_interval),
            params_max_bytes: positive(&var, PARAMS_MAX_BYTES_ENV)?
                .map(|bytes| usize::try_from(bytes).unwrap_or(usize::MAX))
                .unwrap_or(defaults.params_max_bytes),
        })
    }
}

fn positive(
    var: &impl Fn(&str) -> Result<String, std::env::VarError>,
    name: &str,
) -> Result<Option<u64>, String> {
    match var(name) {
        Err(std::env::VarError::NotPresent) => Ok(None),
        Ok(value) => value
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .map(Some)
            .ok_or_else(|| format!("{name} must be a positive integer, got {value:?}")),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{name} must contain valid UTF-8")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(set: &[(&str, &str)]) -> impl Fn(&str) -> Result<String, std::env::VarError> {
        let set: Vec<(String, String)> = set
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        move |name| {
            set.iter()
                .find(|(set_name, _)| set_name == name)
                .map(|(_, value)| value.clone())
                .ok_or(std::env::VarError::NotPresent)
        }
    }

    #[test]
    fn unset_variables_take_the_defaults() {
        assert_eq!(Limits::from_vars(vars(&[])).unwrap(), Limits::default());
    }

    #[test]
    fn set_variables_override_and_invalid_ones_refuse_startup() {
        let limits = Limits::from_vars(vars(&[
            (BUDGET_ENV, "250"),
            (REQUEST_MAX_BYTES_ENV, "4096"),
            (BREAKER_FAILURES_ENV, "3"),
            (PROBE_INTERVAL_ENV, "1000"),
        ]))
        .unwrap();
        assert_eq!(limits.budget, Duration::from_millis(250));
        assert_eq!(limits.request_max_bytes, 4096);
        assert_eq!(limits.breaker_failures, 3);
        assert_eq!(limits.probe_interval, Duration::from_millis(1000));
        for bad in ["0", "-1", "soon"] {
            assert!(Limits::from_vars(vars(&[(BUDGET_ENV, bad)])).is_err());
        }
    }
}
