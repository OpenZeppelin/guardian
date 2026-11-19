//! Configuration of the one-time startup self-heal block
//! (`jobs::self_heal`).

use std::path::PathBuf;

use crate::jobs::self_heal::{SelfHealConfig, SelfHealMode};

/// `off` (the default), `dry-run` or `apply`.
pub const ENV_MODE: &str = "GUARDIAN_SELF_HEAL";

/// Where the recovery file is written.
pub const ENV_RECOVERY_FILE: &str = "GUARDIAN_SELF_HEAL_RECOVERY_FILE";

/// How many accounts the block plans at the same time.
pub const ENV_MAX_CONCURRENT_ACCOUNTS: &str = "GUARDIAN_SELF_HEAL_MAX_CONCURRENT_ACCOUNTS";

pub const DEFAULT_RECOVERY_FILE: &str = "recovery.json";
pub const DEFAULT_MAX_CONCURRENT_ACCOUNTS: usize = 4;
pub const MAX_CONCURRENT_ACCOUNTS: usize = 64;

/// The block does not run.
pub fn default_config() -> SelfHealConfig {
    SelfHealConfig {
        mode: SelfHealMode::Off,
        recovery_file: PathBuf::from(DEFAULT_RECOVERY_FILE),
        max_concurrent_accounts: DEFAULT_MAX_CONCURRENT_ACCOUNTS,
    }
}

/// The defaults with every `GUARDIAN_SELF_HEAL*` value applied. A value
/// that is present but not valid fails startup.
pub fn config_from_env() -> Result<SelfHealConfig, String> {
    config_from_vars(ENV_MODE, ENV_RECOVERY_FILE, ENV_MAX_CONCURRENT_ACCOUNTS)
}

pub fn mode_label(mode: SelfHealMode) -> &'static str {
    match mode {
        SelfHealMode::Off => "off",
        SelfHealMode::DryRun => "dry-run",
        SelfHealMode::Apply => "apply",
    }
}

fn config_from_vars(
    mode_var: &str,
    file_var: &str,
    concurrency_var: &str,
) -> Result<SelfHealConfig, String> {
    let mut config = default_config();
    if let Some(mode) = mode_from_var(mode_var)? {
        config.mode = mode;
    }
    if let Some(file) = path_from_var(file_var)? {
        config.recovery_file = file;
    }
    if let Some(accounts) = bounded_from_var(concurrency_var, MAX_CONCURRENT_ACCOUNTS)? {
        config.max_concurrent_accounts = accounts;
    }
    Ok(config)
}

fn mode_from_var(var_name: &str) -> Result<Option<SelfHealMode>, String> {
    let Some(value) = string_from_var(var_name)? else {
        return Ok(None);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "off" => Ok(Some(SelfHealMode::Off)),
        "dry-run" => Ok(Some(SelfHealMode::DryRun)),
        "apply" => Ok(Some(SelfHealMode::Apply)),
        _ => Err(format!(
            "{var_name} must be 'off', 'dry-run' or 'apply', got {value:?}"
        )),
    }
}

fn path_from_var(var_name: &str) -> Result<Option<PathBuf>, String> {
    let Some(value) = string_from_var(var_name)? else {
        return Ok(None);
    };
    let path = PathBuf::from(value.trim());
    if path.file_name().is_none() {
        return Err(format!("{var_name} must name a file, got {value:?}"));
    }
    Ok(Some(path))
}

/// An integer in `1..=max`, or `None` when the variable is unset.
fn bounded_from_var(var_name: &str, max: usize) -> Result<Option<usize>, String> {
    let Some(value) = string_from_var(var_name)? else {
        return Ok(None);
    };
    match value.trim().parse::<usize>() {
        Ok(parsed) if (1..=max).contains(&parsed) => Ok(Some(parsed)),
        _ => Err(format!(
            "{var_name} must be an integer between 1 and {max}, got {value:?}"
        )),
    }
}

fn string_from_var(var_name: &str) -> Result<Option<String>, String> {
    match std::env::var(var_name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(format!("{var_name} must contain valid UTF-8"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::env_lock::ENV_LOCK;

    const VARS: [&str; 3] = [
        "GUARDIAN_SH_TEST_MODE",
        "GUARDIAN_SH_TEST_FILE",
        "GUARDIAN_SH_TEST_CONCURRENCY",
    ];

    fn from_test_vars() -> Result<SelfHealConfig, String> {
        config_from_vars(VARS[0], VARS[1], VARS[2])
    }

    fn clear_vars() {
        for var in VARS {
            unsafe { std::env::remove_var(var) };
        }
    }

    #[test]
    fn defaults_keep_the_block_off() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear_vars();
        let config = from_test_vars().expect("defaults are valid");
        assert_eq!(config, default_config());
        assert_eq!(config.mode, SelfHealMode::Off);
        assert_eq!(config.recovery_file, PathBuf::from("recovery.json"));
        assert_eq!(config.max_concurrent_accounts, 4);
    }

    #[test]
    fn env_values_apply() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear_vars();
        for (value, mode) in [
            ("off", SelfHealMode::Off),
            (" dry-run ", SelfHealMode::DryRun),
            ("APPLY", SelfHealMode::Apply),
        ] {
            unsafe { std::env::set_var(VARS[0], value) };
            assert_eq!(from_test_vars().expect("valid mode").mode, mode);
        }
        unsafe {
            std::env::set_var(VARS[1], "/var/guardian/recovery.json");
            std::env::set_var(VARS[2], "64");
        }
        let config = from_test_vars().expect("valid values apply");
        assert_eq!(
            config.recovery_file,
            PathBuf::from("/var/guardian/recovery.json")
        );
        assert_eq!(config.max_concurrent_accounts, MAX_CONCURRENT_ACCOUNTS);
        clear_vars();
    }

    #[test]
    fn invalid_values_fail_startup() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear_vars();
        for (var, bad) in [
            (VARS[0], ""),
            (VARS[0], "on"),
            (VARS[0], "dry_run"),
            (VARS[1], ""),
            (VARS[1], "/"),
            (VARS[2], "0"),
            (VARS[2], "65"),
            (VARS[2], "four"),
        ] {
            unsafe { std::env::set_var(var, bad) };
            assert!(from_test_vars().is_err(), "{var}={bad:?} must fail startup");
            unsafe { std::env::remove_var(var) };
        }
    }

    #[test]
    fn mode_labels_match_the_env_values() {
        assert_eq!(mode_label(SelfHealMode::Off), "off");
        assert_eq!(mode_label(SelfHealMode::DryRun), "dry-run");
        assert_eq!(mode_label(SelfHealMode::Apply), "apply");
    }
}
