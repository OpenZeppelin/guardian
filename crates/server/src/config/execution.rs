use std::time::Duration;

use crate::secret::CredentialUrl;

pub const ENV_TX_PROVER_URL: &str = "GUARDIAN_TX_PROVER_URL";
pub const ENV_TX_PROVER_TIMEOUT_SECS: &str = "GUARDIAN_TX_PROVER_TIMEOUT_SECS";
pub const ENV_PROVING_ENABLED: &str = "GUARDIAN_PROVING_ENABLED";
pub const ENV_MAX_PROPOSAL_REQUEST_BYTES: &str = "GUARDIAN_MAX_PROPOSAL_REQUEST_BYTES";
pub const ENV_MAX_ACCOUNT_REQUEST_BYTES: &str = "GUARDIAN_MAX_ACCOUNT_REQUEST_BYTES";
pub const ENV_EXECUTION_LEASE_SECS: &str = "GUARDIAN_EXECUTION_LEASE_SECS";
pub const ENV_EXECUTION_RECONCILE_INTERVAL_SECS: &str =
    "GUARDIAN_EXECUTION_RECONCILE_INTERVAL_SECS";
pub const ENV_EXECUTION_EXPIRATION_HORIZON_BLOCKS: &str =
    "GUARDIAN_EXECUTION_EXPIRATION_HORIZON_BLOCKS";
pub const ENV_EXECUTION_SERIALIZER_ALLOWLIST: &str = "GUARDIAN_EXECUTION_SERIALIZER_ALLOWLIST";

/// The upstream remote-prover client defaults to 10 s, below observed proving
/// times, so Guardian always sets its own.
pub const DEFAULT_TX_PROVER_TIMEOUT_SECS: u32 = 300;
pub const DEFAULT_MAX_PROPOSAL_REQUEST_BYTES: u32 = 256 * 1024;
pub const DEFAULT_MAX_ACCOUNT_REQUEST_BYTES: u32 = 4 * 1024 * 1024;
pub const DEFAULT_EXECUTION_LEASE_SECS: u32 = 120;
pub const DEFAULT_EXECUTION_RECONCILE_INTERVAL_SECS: u32 = 30;
pub const DEFAULT_EXECUTION_EXPIRATION_HORIZON_BLOCKS: u32 = 512;

/// Every built-in Guardian-executable proposal signs this relative
/// transaction expiration, so a horizon below it would refuse all of them.
pub const MIN_EXECUTION_EXPIRATION_HORIZON_BLOCKS: u32 = 256;

/// The `miden-client` version the server's own request codec reads. Stored
/// requests declare the version that serialized them, and only allowlisted
/// versions are decoded, because request serialization carries no version tag.
pub const PINNED_MIDEN_CLIENT_VERSION: &str = "0.17.0-rc.4";

/// The remote prover Guardian delegates proof generation to.
#[derive(Clone, Debug, PartialEq)]
pub struct ProverConfig {
    pub(crate) url: CredentialUrl,
    pub timeout: Duration,
}

/// Why this server does not offer Guardian execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionUnavailable {
    ProverNotConfigured,
    Disabled,
}

impl ExecutionUnavailable {
    /// What an operator reads at startup: why execution is off and how to turn it on.
    pub fn describe(self) -> &'static str {
        match self {
            ExecutionUnavailable::ProverNotConfigured => {
                "no remote prover is configured; set GUARDIAN_TX_PROVER_URL to offer execution"
            }
            ExecutionUnavailable::Disabled => {
                "GUARDIAN_PROVING_ENABLED is false, so execution is switched off"
            }
        }
    }
}

/// How startup reports the execution capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionNotice {
    Enabled,
    /// Execution is off and nothing asked for it.
    Off(ExecutionUnavailable),
    /// A prover is configured, so the operator expects execution, but it is off anyway.
    OffDespiteProver(ExecutionUnavailable),
}

impl ExecutionConfig {
    pub fn startup_notice(&self) -> ExecutionNotice {
        match self.availability() {
            Ok(_) => ExecutionNotice::Enabled,
            Err(reason) if self.prover.is_some() => ExecutionNotice::OffDespiteProver(reason),
            Err(reason) => ExecutionNotice::Off(reason),
        }
    }
}

/// Server-side execution settings, resolved once at startup.
#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionConfig {
    pub prover: Option<ProverConfig>,
    pub proving_enabled: bool,
    pub max_proposal_request_bytes: u32,
    pub max_account_request_bytes: u32,
    pub lease: Duration,
    pub reconcile_interval: Duration,
    pub expiration_horizon_blocks: u32,
    pub serializer_allowlist: Vec<String>,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            prover: None,
            proving_enabled: true,
            max_proposal_request_bytes: DEFAULT_MAX_PROPOSAL_REQUEST_BYTES,
            max_account_request_bytes: DEFAULT_MAX_ACCOUNT_REQUEST_BYTES,
            lease: Duration::from_secs(u64::from(DEFAULT_EXECUTION_LEASE_SECS)),
            reconcile_interval: Duration::from_secs(u64::from(
                DEFAULT_EXECUTION_RECONCILE_INTERVAL_SECS,
            )),
            expiration_horizon_blocks: DEFAULT_EXECUTION_EXPIRATION_HORIZON_BLOCKS,
            serializer_allowlist: vec![PINNED_MIDEN_CLIENT_VERSION.to_string()],
        }
    }
}

impl ExecutionConfig {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|key| match std::env::var(key) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(format!("{key} must contain valid UTF-8"))
            }
        })
    }

    pub fn from_lookup(
        lookup: impl Fn(&str) -> Result<Option<String>, String>,
    ) -> Result<Self, String> {
        let timeout = positive_u32(
            &lookup,
            ENV_TX_PROVER_TIMEOUT_SECS,
            DEFAULT_TX_PROVER_TIMEOUT_SECS,
        )?;
        let prover = non_blank(lookup(ENV_TX_PROVER_URL)?).map(|url| ProverConfig {
            url: CredentialUrl::new(url),
            timeout: Duration::from_secs(u64::from(timeout)),
        });
        let proving_enabled = match non_blank(lookup(ENV_PROVING_ENABLED)?) {
            Some(value) => value.trim().parse::<bool>().map_err(|_| {
                format!("{ENV_PROVING_ENABLED} must be 'true' or 'false', got '{value}'")
            })?,
            None => true,
        };
        let expiration_horizon_blocks = positive_u32(
            &lookup,
            ENV_EXECUTION_EXPIRATION_HORIZON_BLOCKS,
            DEFAULT_EXECUTION_EXPIRATION_HORIZON_BLOCKS,
        )?;
        if expiration_horizon_blocks < MIN_EXECUTION_EXPIRATION_HORIZON_BLOCKS {
            return Err(format!(
                "{ENV_EXECUTION_EXPIRATION_HORIZON_BLOCKS} must be at least \
                 {MIN_EXECUTION_EXPIRATION_HORIZON_BLOCKS}, the built-in transaction \
                 expiration, got {expiration_horizon_blocks}"
            ));
        }
        let serializer_allowlist = match non_blank(lookup(ENV_EXECUTION_SERIALIZER_ALLOWLIST)?) {
            Some(csv) => parse_allowlist(&csv)?,
            None => vec![PINNED_MIDEN_CLIENT_VERSION.to_string()],
        };
        Ok(Self {
            prover,
            proving_enabled,
            max_proposal_request_bytes: positive_u32(
                &lookup,
                ENV_MAX_PROPOSAL_REQUEST_BYTES,
                DEFAULT_MAX_PROPOSAL_REQUEST_BYTES,
            )?,
            max_account_request_bytes: positive_u32(
                &lookup,
                ENV_MAX_ACCOUNT_REQUEST_BYTES,
                DEFAULT_MAX_ACCOUNT_REQUEST_BYTES,
            )?,
            lease: Duration::from_secs(u64::from(positive_u32(
                &lookup,
                ENV_EXECUTION_LEASE_SECS,
                DEFAULT_EXECUTION_LEASE_SECS,
            )?)),
            reconcile_interval: Duration::from_secs(u64::from(positive_u32(
                &lookup,
                ENV_EXECUTION_RECONCILE_INTERVAL_SECS,
                DEFAULT_EXECUTION_RECONCILE_INTERVAL_SECS,
            )?)),
            expiration_horizon_blocks,
            serializer_allowlist,
        })
    }

    /// Whether this server can execute proposals, and if not, why.
    pub fn availability(&self) -> Result<&ProverConfig, ExecutionUnavailable> {
        if !self.proving_enabled {
            return Err(ExecutionUnavailable::Disabled);
        }
        self.prover
            .as_ref()
            .ok_or(ExecutionUnavailable::ProverNotConfigured)
    }

    pub fn admits_serializer(&self, serializer_id: &str) -> bool {
        self.serializer_allowlist
            .iter()
            .any(|allowed| allowed == serializer_id)
    }
}

fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn positive_u32(
    lookup: &impl Fn(&str) -> Result<Option<String>, String>,
    key: &str,
    default: u32,
) -> Result<u32, String> {
    match non_blank(lookup(key)?) {
        Some(raw) => match raw.trim().parse::<u32>() {
            Ok(0) => Err(format!("{key} must be a positive integer, got 0")),
            Ok(value) => Ok(value),
            Err(_) => Err(format!("{key} must be a positive integer, got {raw:?}")),
        },
        None => Ok(default),
    }
}

fn parse_allowlist(csv: &str) -> Result<Vec<String>, String> {
    let entries: Vec<String> = csv
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect();
    if entries.is_empty() {
        return Err(format!(
            "{ENV_EXECUTION_SERIALIZER_ALLOWLIST} must name at least one miden-client version or be unset"
        ));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn a_configured_prover_that_cannot_be_used_is_reported_as_a_warning() {
        let disabled = ExecutionConfig {
            prover: ExecutionConfig::from_lookup(|key| {
                Ok((key == ENV_TX_PROVER_URL).then(|| "https://prover.example".to_string()))
            })
            .unwrap()
            .prover,
            proving_enabled: false,
            ..ExecutionConfig::default()
        };
        assert!(matches!(
            disabled.startup_notice(),
            ExecutionNotice::OffDespiteProver(_)
        ));
        assert!(matches!(
            ExecutionConfig::default().startup_notice(),
            ExecutionNotice::Off(_)
        ));
    }

    fn config_from(vars: &[(&str, &str)]) -> Result<ExecutionConfig, String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        ExecutionConfig::from_lookup(|key| Ok(vars.get(key).cloned()))
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let config = config_from(&[]).unwrap();
        assert_eq!(config, ExecutionConfig::default());
        assert!(config.prover.is_none());
        assert!(config.proving_enabled);
        assert_eq!(config.expiration_horizon_blocks, 512);
        assert_eq!(config.lease, Duration::from_secs(120));
        assert_eq!(config.reconcile_interval, Duration::from_secs(30));
    }

    #[test]
    fn prover_timeout_defaults_to_three_hundred_seconds() {
        let config = config_from(&[(ENV_TX_PROVER_URL, "https://prover.example:50051")]).unwrap();
        let prover = config.prover.expect("prover configured");
        assert_eq!(prover.timeout, Duration::from_secs(300));
        assert_eq!(DEFAULT_TX_PROVER_TIMEOUT_SECS, 300);
    }

    #[test]
    fn prover_url_is_redacted_in_debug_output() {
        let config = config_from(&[(
            ENV_TX_PROVER_URL,
            "https://user:secret@prover.example:50051/path?token=abc",
        )])
        .unwrap();
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("token=abc"));
        assert!(rendered.contains("prover.example"));
    }

    #[test]
    fn horizon_below_the_built_in_transaction_expiration_is_rejected() {
        let error = config_from(&[(ENV_EXECUTION_EXPIRATION_HORIZON_BLOCKS, "255")]).unwrap_err();
        assert!(error.contains(ENV_EXECUTION_EXPIRATION_HORIZON_BLOCKS));
        assert!(config_from(&[(ENV_EXECUTION_EXPIRATION_HORIZON_BLOCKS, "256")]).is_ok());
    }

    #[test]
    fn zero_and_malformed_integers_are_rejected_with_the_variable_name() {
        for key in [
            ENV_TX_PROVER_TIMEOUT_SECS,
            ENV_MAX_PROPOSAL_REQUEST_BYTES,
            ENV_MAX_ACCOUNT_REQUEST_BYTES,
            ENV_EXECUTION_LEASE_SECS,
            ENV_EXECUTION_RECONCILE_INTERVAL_SECS,
        ] {
            assert!(config_from(&[(key, "0")]).unwrap_err().contains(key));
            assert!(config_from(&[(key, "ten")]).unwrap_err().contains(key));
        }
    }

    #[test]
    fn proving_enabled_accepts_only_booleans() {
        assert!(
            !config_from(&[(ENV_PROVING_ENABLED, "false")])
                .unwrap()
                .proving_enabled
        );
        assert!(
            config_from(&[(ENV_PROVING_ENABLED, "yes")])
                .unwrap_err()
                .contains(ENV_PROVING_ENABLED)
        );
    }

    #[test]
    fn serializer_allowlist_defaults_to_the_pinned_client_and_parses_csv() {
        let config = config_from(&[]).unwrap();
        assert!(config.admits_serializer(PINNED_MIDEN_CLIENT_VERSION));
        assert!(!config.admits_serializer("0.17.0-rc.3"));

        let config =
            config_from(&[(ENV_EXECUTION_SERIALIZER_ALLOWLIST, " 0.17.0-rc.4 , 0.17.0 ")]).unwrap();
        assert!(config.admits_serializer("0.17.0"));
        assert!(config_from(&[(ENV_EXECUTION_SERIALIZER_ALLOWLIST, " , ")]).is_err());
    }

    #[test]
    fn pinned_client_version_matches_the_workspace_lockfile() {
        let lockfile = include_str!("../../../../Cargo.lock");
        let pinned = lockfile
            .split("[[package]]")
            .find(|entry| entry.contains("\nname = \"miden-client\"\n"))
            .and_then(|entry| {
                entry
                    .lines()
                    .find_map(|line| line.strip_prefix("version = \""))
                    .map(|version| version.trim_end_matches('"').to_string())
            })
            .expect("miden-client is in Cargo.lock");
        assert_eq!(pinned, PINNED_MIDEN_CLIENT_VERSION);
    }

    #[test]
    fn availability_reports_why_execution_is_off() {
        let unconfigured = config_from(&[]).unwrap();
        let disabled = config_from(&[
            (ENV_TX_PROVER_URL, "https://prover.example"),
            (ENV_PROVING_ENABLED, "false"),
        ])
        .unwrap();
        let configured = config_from(&[(ENV_TX_PROVER_URL, "https://prover.example")]).unwrap();
        assert_eq!(
            unconfigured.availability().unwrap_err(),
            ExecutionUnavailable::ProverNotConfigured
        );
        assert_eq!(
            disabled.availability().unwrap_err(),
            ExecutionUnavailable::Disabled
        );
        assert!(configured.availability().is_ok());
    }
}
