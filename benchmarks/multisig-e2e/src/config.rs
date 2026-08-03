use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use miden_multisig_client::{Endpoint, ProverConfig};
use serde::Deserialize;

/// Where the generator proves transactions.
///
/// `remote` delegates to the network's shared prover, which is what a real
/// client does. `local` proves in-process. A URL points at a prover you run
/// yourself -- the option that makes proving CPU a budgeted quantity, since a
/// container takes a limit and an in-process prover does not.
///
/// Above a handful of writers the shared prover measures itself rather than
/// GUARDIAN: a prover instance proves one transaction at a time, so concurrency
/// beyond its capacity queues and eventually times out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProverChoice {
    #[default]
    Remote,
    Local,
    Service(String),
}

impl<'de> Deserialize<'de> for ProverChoice {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "remote" => Self::Remote,
            "local" => Self::Local,
            _ => Self::Service(value),
        })
    }
}

impl ProverChoice {
    /// How the run should describe its proving setup in the summary.
    pub fn label(&self) -> String {
        match self {
            Self::Remote => "remote (network default)".to_string(),
            Self::Local => "local (in-process)".to_string(),
            Self::Service(url) => format!("service ({url})"),
        }
    }
}

impl ProverChoice {
    /// The SDK prover configuration this choice stands for. Fails only for a
    /// `Service` URL the SDK refuses, which a run should surface before it
    /// provisions anything.
    pub fn to_prover_config(&self) -> Result<ProverConfig> {
        match self {
            Self::Remote => Ok(ProverConfig::new()),
            Self::Local => Ok(ProverConfig::new().with_local()),
            Self::Service(url) => ProverConfig::new()
                .with_url(url)
                .with_context(|| format!("invalid prover service url '{url}'")),
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
pub struct RunConfig {
    pub accounts_file: PathBuf,
    pub faucet_id: String,
    #[serde(default = "default_operations")]
    pub operations: u64,
    #[serde(default = "default_amount")]
    pub amount: u64,
    #[serde(default = "default_consume_probability")]
    pub consume_probability: f64,
    #[serde(default = "default_seed")]
    pub seed: u64,
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_proposal_retry_interval_ms")]
    pub proposal_retry_interval_ms: u64,
    #[serde(default = "default_proposal_retry_timeout_seconds")]
    pub proposal_retry_timeout_seconds: u64,
    #[serde(default)]
    pub max_duration_seconds: Option<u64>,
    #[serde(default)]
    pub prover: ProverChoice,
    #[serde(default = "default_artifacts_dir")]
    pub artifacts_dir: PathBuf,
}

/// Configuration for the N-writer scale runner.
///
/// Shares the account fixture, faucet, and retry/poll settings with
/// [`RunConfig`] so one TOML file drives both runners, and adds the two knobs
/// the issue #317 write target needs: how many accounts to drive concurrently,
/// and for how long. The lifecycle runner's `operations` / `consume_probability`
/// / `seed` have no meaning here — a scale run is bounded by time, not by a
/// fixed operation count, because per-writer throughput is an observed property
/// bounded by canonicalization rather than something the profile chooses.
#[derive(Debug, Clone, Deserialize)]
pub struct ScaleConfig {
    pub accounts_file: PathBuf,
    pub faucet_id: String,
    /// Concurrent writers. Must not exceed the fixture's account count.
    pub writers: usize,
    #[serde(default = "default_scale_duration_seconds")]
    pub duration_seconds: u64,
    #[serde(default = "default_amount")]
    pub amount: u64,
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_proposal_retry_interval_ms")]
    pub proposal_retry_interval_ms: u64,
    #[serde(default = "default_proposal_retry_timeout_seconds")]
    pub proposal_retry_timeout_seconds: u64,
    #[serde(default)]
    pub prover: ProverChoice,
    #[serde(default = "default_artifacts_dir")]
    pub artifacts_dir: PathBuf,
}

fn default_scale_duration_seconds() -> u64 {
    300
}

impl ScaleConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read scale config {}", path.display()))?;
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse scale config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.writers < 2 {
            bail!("writers must be at least 2 to form a transfer ring");
        }
        if self.duration_seconds == 0 {
            bail!("duration_seconds must be greater than zero");
        }
        if self.amount == 0 {
            bail!("amount must be greater than zero");
        }
        if self.poll_interval_ms == 0 {
            bail!("poll_interval_ms must be greater than zero");
        }
        if self.timeout_seconds == 0 {
            bail!("timeout_seconds must be greater than zero");
        }
        // Matches RunConfig: a zero interval spins a hot retry loop, and a zero
        // timeout silently disables retries rather than configuring them.
        if self.proposal_retry_interval_ms == 0 || self.proposal_retry_timeout_seconds == 0 {
            bail!("proposal retry interval and timeout must be greater than zero");
        }
        Ok(())
    }

    /// Project onto a [`RunConfig`] so the proposal/execution helpers are shared
    /// rather than duplicated. Fields with no scale meaning take placeholders.
    pub fn to_run_config(&self) -> RunConfig {
        RunConfig {
            accounts_file: self.accounts_file.clone(),
            faucet_id: self.faucet_id.clone(),
            operations: 1,
            amount: self.amount,
            consume_probability: 0.0,
            seed: 0,
            poll_interval_ms: self.poll_interval_ms,
            timeout_seconds: self.timeout_seconds,
            proposal_retry_interval_ms: self.proposal_retry_interval_ms,
            proposal_retry_timeout_seconds: self.proposal_retry_timeout_seconds,
            max_duration_seconds: None,
            prover: self.prover.clone(),
            artifacts_dir: self.artifacts_dir.clone(),
        }
    }
}

impl RunConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read benchmark config {}", path.display()))?;
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse benchmark config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.operations == 0 {
            bail!("operations must be greater than zero");
        }
        if self.amount == 0 {
            bail!("amount must be greater than zero");
        }
        if !(0.0..=1.0).contains(&self.consume_probability) {
            bail!("consume_probability must be between 0 and 1");
        }
        if self.poll_interval_ms == 0 || self.timeout_seconds == 0 {
            bail!("poll_interval_ms and timeout_seconds must be greater than zero");
        }
        if self.proposal_retry_interval_ms == 0 || self.proposal_retry_timeout_seconds == 0 {
            bail!(
                "proposal_retry_interval_ms and proposal_retry_timeout_seconds must be greater than zero"
            );
        }
        if self.max_duration_seconds == Some(0) {
            bail!("max_duration_seconds must be greater than zero when set");
        }
        Ok(())
    }
}

pub fn parse_miden_endpoint(input: &str) -> Result<Endpoint> {
    let (protocol, authority) = input
        .split_once("://")
        .ok_or_else(|| anyhow!("Miden endpoint must start with http:// or https://"))?;
    if protocol != "http" && protocol != "https" {
        bail!("unsupported Miden endpoint protocol '{protocol}'");
    }
    if authority.is_empty() || authority.contains('/') {
        bail!("Miden endpoint must contain only a host and optional port");
    }

    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => {
            let port = port
                .parse::<u16>()
                .with_context(|| format!("invalid Miden endpoint port '{port}'"))?;
            (host.to_string(), Some(port))
        }
        _ => (authority.to_string(), None),
    };
    Ok(Endpoint::new(protocol.to_string(), host, port))
}

fn default_operations() -> u64 {
    300
}

fn default_amount() -> u64 {
    1
}

fn default_consume_probability() -> f64 {
    0.5
}

fn default_seed() -> u64 {
    42
}

fn default_poll_interval_ms() -> u64 {
    1_000
}

fn default_timeout_seconds() -> u64 {
    180
}

fn default_proposal_retry_interval_ms() -> u64 {
    1_000
}

fn default_proposal_retry_timeout_seconds() -> u64 {
    180
}

fn default_artifacts_dir() -> PathBuf {
    PathBuf::from("benchmarks/multisig-e2e/reports")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_endpoint_with_port() {
        let endpoint = parse_miden_endpoint("http://localhost:57291").unwrap();
        assert_eq!(endpoint.protocol(), "http");
        assert_eq!(endpoint.host(), "localhost");
        assert_eq!(endpoint.port(), Some(57291));
    }

    #[test]
    fn parses_endpoint_without_port() {
        let endpoint = parse_miden_endpoint("https://rpc.devnet.miden.io").unwrap();
        assert_eq!(endpoint.protocol(), "https");
        assert_eq!(endpoint.host(), "rpc.devnet.miden.io");
        assert_eq!(endpoint.port(), None);
    }

    #[test]
    fn rejects_endpoint_path() {
        assert!(parse_miden_endpoint("https://rpc.devnet.miden.io/path").is_err());
    }
}
