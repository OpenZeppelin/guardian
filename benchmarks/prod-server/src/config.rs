use crate::model::AuthScheme;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Grpc,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunConfig {
    pub profile_name: String,
    pub guardian_endpoint: String,
    pub transport: Transport,
    pub duration_seconds: u64,
    pub warmup_seconds: u64,
    pub users: u32,
    pub accounts_per_user: u32,
    pub deployment_shape: Option<String>,
    pub operation_mix: OperationMix,
    pub scheme_distribution: SchemeDistribution,
    pub canonicalization: CanonicalizationConfig,
    pub cleanup: CleanupConfig,
    pub aws: AwsConfig,
    pub artifacts_dir: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationMix {
    pub read_operation: ReadOperation,
    pub reads_per_push: u32,
    #[serde(default)]
    pub retire_after_first_successful_push: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadOperation {
    GetState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SchemeDistribution {
    pub falcon_percent: u8,
    pub ecdsa_percent: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CanonicalizationConfig {
    pub sample_rate: f64,
    pub poll_interval_ms: u64,
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CleanupConfig {
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AwsConfig {
    pub profile: Option<String>,
    pub region: String,
    pub ecs_cluster: String,
    pub ecs_service: String,
    pub ecs_container: Option<String>,
}

impl RunConfig {
    pub fn load_from_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read benchmark profile {}", path.display()))?;
        let config: Self = toml::from_str(&raw)
            .with_context(|| format!("failed to parse benchmark profile {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.profile_name.trim().is_empty() {
            bail!("profile_name must not be empty");
        }
        if self.guardian_endpoint.trim().is_empty() {
            bail!("guardian_endpoint must not be empty");
        }
        if self.duration_seconds == 0 {
            bail!("duration_seconds must be greater than 0");
        }
        if self.users == 0 {
            bail!("users must be greater than 0");
        }
        if self.accounts_per_user != 1 {
            bail!("accounts_per_user must be exactly 1 for phase 1");
        }
        if self.scheme_distribution.falcon_percent as u16
            + self.scheme_distribution.ecdsa_percent as u16
            != 100
        {
            bail!("scheme_distribution percentages must sum to 100");
        }
        if !(0.0..=1.0).contains(&self.canonicalization.sample_rate) {
            bail!("canonicalization.sample_rate must be in [0, 1]");
        }
        if self.canonicalization.poll_interval_ms == 0 {
            bail!("canonicalization.poll_interval_ms must be greater than 0");
        }
        if self.canonicalization.timeout_seconds == 0 {
            bail!("canonicalization.timeout_seconds must be greater than 0");
        }
        if self.aws.region.trim().is_empty() {
            bail!("aws.region must not be empty");
        }
        if self.aws.ecs_cluster.trim().is_empty() {
            bail!("aws.ecs_cluster must not be empty");
        }
        if self.aws.ecs_service.trim().is_empty() {
            bail!("aws.ecs_service must not be empty");
        }
        Ok(())
    }

    pub fn active_schemes(&self) -> Vec<AuthScheme> {
        let mut schemes = Vec::new();
        if self.scheme_distribution.falcon_percent > 0 {
            schemes.push(AuthScheme::Falcon);
        }
        if self.scheme_distribution.ecdsa_percent > 0 {
            schemes.push(AuthScheme::Ecdsa);
        }
        schemes
    }

    pub fn normalized_guardian_endpoint(&self) -> String {
        normalize_endpoint(&self.guardian_endpoint)
    }
}

fn normalize_endpoint(endpoint: &str) -> String {
    if let Some(rest) = endpoint.strip_prefix("https://") {
        return normalize_with_scheme("https://", rest, 443);
    }
    if let Some(rest) = endpoint.strip_prefix("http://") {
        return normalize_with_scheme("http://", rest, 80);
    }
    endpoint.to_string()
}

fn normalize_with_scheme(prefix: &str, rest: &str, default_port: u16) -> String {
    let (authority, suffix) = match rest.split_once('/') {
        Some((authority, suffix)) => (authority, format!("/{}", suffix)),
        None => (rest, String::new()),
    };
    if authority.contains(':') {
        return format!("{prefix}{authority}{suffix}");
    }
    format!("{prefix}{authority}:{default_port}{suffix}")
}
