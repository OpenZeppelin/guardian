use std::time::Duration;

use crate::secret::CredentialUrl;

pub const ENV_TX_PROVER_URL: &str = "GUARDIAN_TX_PROVER_URL";
pub const ENV_TX_PROVER_TIMEOUT_SECS: &str = "GUARDIAN_TX_PROVER_TIMEOUT_SECS";
pub const ENV_TX_PROVER_MAX_CONCURRENT: &str = "GUARDIAN_TX_PROVER_MAX_CONCURRENT";
pub const ENV_PROVING_ENABLED: &str = "GUARDIAN_PROVING_ENABLED";
pub const ENV_MAX_PROPOSAL_REQUEST_BYTES: &str = "GUARDIAN_MAX_PROPOSAL_REQUEST_BYTES";
pub const ENV_MAX_ACCOUNT_REQUEST_BYTES: &str = "GUARDIAN_MAX_ACCOUNT_REQUEST_BYTES";
pub const ENV_EXECUTION_LEASE_SECS: &str = "GUARDIAN_EXECUTION_LEASE_SECS";
pub const ENV_EXECUTION_RECONCILE_INTERVAL_SECS: &str =
    "GUARDIAN_EXECUTION_RECONCILE_INTERVAL_SECS";
pub const ENV_EXECUTION_EXPIRATION_HORIZON_BLOCKS: &str =
    "GUARDIAN_EXECUTION_EXPIRATION_HORIZON_BLOCKS";
pub const ENV_EXECUTION_MAX_CONCURRENT: &str = "GUARDIAN_EXECUTION_MAX_CONCURRENT";
pub const ENV_EXECUTION_RECORD_RETENTION_DAYS: &str = "GUARDIAN_EXECUTION_RECORD_RETENTION_DAYS";

/// The upstream remote-prover client defaults to 10 s, below observed proving
/// times, so Guardian always sets its own.
pub const DEFAULT_TX_PROVER_TIMEOUT_SECS: u32 = 300;
pub const DEFAULT_MAX_PROPOSAL_REQUEST_BYTES: u32 = 256 * 1024;
pub const DEFAULT_MAX_ACCOUNT_REQUEST_BYTES: u32 = 4 * 1024 * 1024;
pub const DEFAULT_EXECUTION_LEASE_SECS: u32 = 120;
pub const DEFAULT_EXECUTION_RECONCILE_INTERVAL_SECS: u32 = 30;
pub const DEFAULT_EXECUTION_EXPIRATION_HORIZON_BLOCKS: u32 = 512;
/// Executions one process holds at once, a safety bound sized to memory: each holds an account
/// reservation, its chain view and its proving inputs, and the ones beyond the prover cap wait in
/// memory for a proof permit rather than reaching the prover.
pub const DEFAULT_EXECUTION_MAX_CONCURRENT: u32 = 64;
pub const DEFAULT_EXECUTION_RECORD_RETENTION_DAYS: u32 = 30;

/// Every built-in Guardian-executable proposal signs this relative
/// transaction expiration, so a horizon below it would refuse all of them.
pub const MIN_EXECUTION_EXPIRATION_HORIZON_BLOCKS: u32 = 256;

/// The shortest record retention accepted. A finished attempt must outlive the approval window
/// its proposal can still be executed in, 28,800 blocks or about one day by default, so a client
/// waiting on it still reads its outcome.
pub const MIN_EXECUTION_RECORD_RETENTION_DAYS: u32 = 2;

/// The longest execution lease accepted. A worker that stops renewing holds the account until
/// its lease lapses, so a very long lease turns one crash into a long outage for the account.
pub const MAX_EXECUTION_LEASE_SECS: u32 = 3600;

/// The remote prover Guardian delegates proof generation to.
#[derive(Clone, Debug, PartialEq)]
pub struct ProverConfig {
    pub(crate) url: CredentialUrl,
    pub timeout: Duration,
    /// Proofs this process has at the prover at once; `None` leaves them bounded only by the
    /// executions it holds. Set for a shared or small prover, which times out when overloaded.
    pub max_concurrent: Option<u32>,
}

/// How long finished execution attempts are kept before the retention sweep removes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordRetention {
    KeepForever,
    Days(u32),
}

impl RecordRetention {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().parse::<u32>() {
            Ok(0) => Ok(RecordRetention::KeepForever),
            Ok(days) if days < MIN_EXECUTION_RECORD_RETENTION_DAYS => Err(format!(
                "{ENV_EXECUTION_RECORD_RETENTION_DAYS} must be 0 (keep forever) or at least \
                 {MIN_EXECUTION_RECORD_RETENTION_DAYS} days, so finished attempts outlive the \
                 approval window of about one day, got {days}"
            )),
            Ok(days) => Ok(RecordRetention::Days(days)),
            Err(_) => Err(format!(
                "{ENV_EXECUTION_RECORD_RETENTION_DAYS} must be a whole number of days, got {raw:?}"
            )),
        }
    }
}

/// Why this server does not offer Guardian execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionUnavailable {
    ProverNotConfigured,
    Disabled,
    /// The server runs without canonicalization, which execution commits through.
    CanonicalizationDisabled,
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
            ExecutionUnavailable::CanonicalizationDisabled => {
                "canonicalization is off, and execution commits through it"
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
    pub max_concurrent_executions: u32,
    pub record_retention: RecordRetention,
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
            max_concurrent_executions: DEFAULT_EXECUTION_MAX_CONCURRENT,
            record_retention: RecordRetention::Days(DEFAULT_EXECUTION_RECORD_RETENTION_DAYS),
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
        let max_concurrent_proofs = match non_blank(lookup(ENV_TX_PROVER_MAX_CONCURRENT)?) {
            Some(_) => Some(positive_u32(&lookup, ENV_TX_PROVER_MAX_CONCURRENT, 1)?),
            None => None,
        };
        let prover = match non_blank(lookup(ENV_TX_PROVER_URL)?)
            .map(|url| CredentialUrl::new(url.trim().to_string()))
        {
            Some(url) => {
                ensure_prover_url(url.expose_secret())?;
                Some(ProverConfig {
                    url,
                    timeout: Duration::from_secs(u64::from(timeout)),
                    max_concurrent: max_concurrent_proofs,
                })
            }
            None => None,
        };
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
        let lease_secs = positive_u32(
            &lookup,
            ENV_EXECUTION_LEASE_SECS,
            DEFAULT_EXECUTION_LEASE_SECS,
        )?;
        if lease_secs > MAX_EXECUTION_LEASE_SECS {
            return Err(format!(
                "{ENV_EXECUTION_LEASE_SECS} must be at most {MAX_EXECUTION_LEASE_SECS}, got {lease_secs}"
            ));
        }
        let reconcile_secs = positive_u32(
            &lookup,
            ENV_EXECUTION_RECONCILE_INTERVAL_SECS,
            DEFAULT_EXECUTION_RECONCILE_INTERVAL_SECS,
        )?;
        if reconcile_secs >= lease_secs {
            return Err(format!(
                "{ENV_EXECUTION_RECONCILE_INTERVAL_SECS} ({reconcile_secs}) must be below \
                 {ENV_EXECUTION_LEASE_SECS} ({lease_secs}), or an abandoned attempt outlives \
                 several leases before it is noticed"
            ));
        }
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
            lease: Duration::from_secs(u64::from(lease_secs)),
            reconcile_interval: Duration::from_secs(u64::from(reconcile_secs)),
            expiration_horizon_blocks,
            max_concurrent_executions: positive_u32(
                &lookup,
                ENV_EXECUTION_MAX_CONCURRENT,
                DEFAULT_EXECUTION_MAX_CONCURRENT,
            )?,
            record_retention: match non_blank(lookup(ENV_EXECUTION_RECORD_RETENTION_DAYS)?) {
                Some(raw) => RecordRetention::parse(&raw)?,
                None => RecordRetention::Days(DEFAULT_EXECUTION_RECORD_RETENTION_DAYS),
            },
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
}

/// Refuses a prover URL the prover client could not connect to. The message never repeats the
/// URL, which can carry credentials.
/// The prover client sends neither userinfo nor a query, so a URL carrying them would only look
/// authenticated; access control for a private prover belongs to the network in front of it.
fn ensure_prover_url(url: &str) -> Result<(), String> {
    match url::Url::parse(url.trim()) {
        Ok(parsed) if !matches!(parsed.scheme(), "http" | "https") || !parsed.has_host() => Err(
            format!("{ENV_TX_PROVER_URL} must be an http or https URL with a host"),
        ),
        Ok(parsed)
            if !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some() =>
        {
            Err(format!(
                "{ENV_TX_PROVER_URL} must not carry credentials, a query or a fragment: the \
                 prover client sends none of them"
            ))
        }
        Ok(_) => Ok(()),
        Err(error) => Err(format!("{ENV_TX_PROVER_URL} is not a valid URL: {error}")),
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
    fn a_malformed_prover_url_is_refused_without_echoing_it() {
        for url in [
            "not a url",
            "ftp://prover:secret@host",
            "https://",
            "https://user:secret@prover.example:50051",
            "https://prover.example:50051/?token=secret",
            "https://prover.example:50051/#secret",
        ] {
            let error = config_from(&[(ENV_TX_PROVER_URL, url)]).unwrap_err();
            assert!(error.contains(ENV_TX_PROVER_URL), "{error}");
            assert!(!error.contains("secret"), "{error}");
        }
        assert!(config_from(&[(ENV_TX_PROVER_URL, "https://prover.example:50051")]).is_ok());
        let padded = config_from(&[(ENV_TX_PROVER_URL, "  https://prover.example:50051  ")])
            .unwrap()
            .prover
            .expect("a prover is configured");
        assert_eq!(padded.url.expose_secret(), "https://prover.example:50051");
    }

    #[test]
    fn the_lease_is_bounded_and_outlasts_the_reconcile_interval() {
        assert!(config_from(&[(ENV_EXECUTION_LEASE_SECS, "3601")]).is_err());
        assert!(config_from(&[(ENV_EXECUTION_LEASE_SECS, "3600")]).is_ok());
        assert!(
            config_from(&[
                (ENV_EXECUTION_LEASE_SECS, "30"),
                (ENV_EXECUTION_RECONCILE_INTERVAL_SECS, "30"),
            ])
            .is_err()
        );
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
        assert_eq!(config.max_concurrent_executions, 64);
        assert_eq!(config.record_retention, RecordRetention::Days(30));
    }

    #[test]
    fn record_retention_is_zero_for_forever_or_outlasts_the_approval_window() {
        let retention = |raw: &str| config_from(&[(ENV_EXECUTION_RECORD_RETENTION_DAYS, raw)]);
        assert_eq!(
            retention("0").unwrap().record_retention,
            RecordRetention::KeepForever
        );
        assert_eq!(
            retention(" 2 ").unwrap().record_retention,
            RecordRetention::Days(2)
        );
        assert_eq!(
            retention("").unwrap().record_retention,
            RecordRetention::Days(DEFAULT_EXECUTION_RECORD_RETENTION_DAYS)
        );
        for refused in ["1", "-1", "thirty", "1.5"] {
            let error = retention(refused).unwrap_err();
            assert!(
                error.contains(ENV_EXECUTION_RECORD_RETENTION_DAYS),
                "{error}"
            );
        }
        assert!(retention("1").unwrap_err().contains("approval window"));
    }

    #[test]
    fn the_execution_cap_must_be_positive() {
        assert!(config_from(&[(ENV_EXECUTION_MAX_CONCURRENT, "0")]).is_err());
        assert!(config_from(&[(ENV_EXECUTION_MAX_CONCURRENT, "many")]).is_err());
        assert_eq!(
            config_from(&[(ENV_EXECUTION_MAX_CONCURRENT, "3")])
                .unwrap()
                .max_concurrent_executions,
            3
        );
    }

    #[test]
    fn prover_timeout_defaults_to_three_hundred_seconds() {
        let config = config_from(&[(ENV_TX_PROVER_URL, "https://prover.example:50051")]).unwrap();
        let prover = config.prover.expect("prover configured");
        assert_eq!(prover.timeout, Duration::from_secs(300));
        assert_eq!(
            prover.max_concurrent, None,
            "proofs are not limited unless asked"
        );
        let limited = config_from(&[
            (ENV_TX_PROVER_URL, "https://prover.example:50051"),
            (ENV_TX_PROVER_MAX_CONCURRENT, "4"),
        ])
        .unwrap();
        assert_eq!(limited.prover.unwrap().max_concurrent, Some(4));
        assert_eq!(DEFAULT_TX_PROVER_TIMEOUT_SECS, 300);
    }

    #[test]
    fn prover_url_is_redacted_in_debug_output() {
        let config = config_from(&[(
            ENV_TX_PROVER_URL,
            "https://prover.example:50051/private-route",
        )])
        .unwrap();
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("private-route"));
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
            ENV_TX_PROVER_MAX_CONCURRENT,
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
