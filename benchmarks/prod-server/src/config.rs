use crate::model::AuthScheme;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    pub load_model: LoadModel,
    pub operation_mix: OperationMix,
    pub scheme_distribution: SchemeDistribution,
    pub canonicalization: CanonicalizationConfig,
    pub cleanup: CleanupConfig,
    pub aws: AwsConfig,
    pub artifacts_dir: PathBuf,
}

/// How offered load relates to the user count.
///
/// The two are not interchangeable and the difference is more than an order of
/// magnitude at the same `users`, so a profile states which one it means rather
/// than leaving it to whoever reads the result (spec FR-003, FR-003b).
///
/// `closed_loop` is the in-flight-saturation model: a user issues its next
/// operation the instant the previous one returns, so `users` is requests in
/// flight. It finds ceilings, and FR-003c requires its results be reported as
/// ceilings rather than as target verdicts.
///
/// `paced` is the target's model: a user issues one operation every
/// `read_interval_ms` regardless of how fast the last one returned, so `users`
/// is a client population and offered load is `users / interval`. This is the
/// only model that can express "20,000 readers at one read per 10s".
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "model", rename_all = "snake_case")]
pub enum LoadModel {
    ClosedLoop,
    Paced { read_interval_ms: u64 },
}

impl LoadModel {
    pub fn interval(&self) -> Option<Duration> {
        match self {
            Self::ClosedLoop => None,
            Self::Paced { read_interval_ms } => Some(Duration::from_millis(*read_interval_ms)),
        }
    }

    pub fn read_interval_ms(&self) -> Option<u64> {
        match self {
            Self::ClosedLoop => None,
            Self::Paced { read_interval_ms } => Some(*read_interval_ms),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum OperationMix {
    ReadOnly,
    PushOnly {
        retire_after_first_successful_push: bool,
    },
    Mixed {
        reads_per_push: u32,
        retire_after_first_successful_push: bool,
    },
}

impl OperationMix {
    pub fn retires_after_first_successful_push(&self) -> bool {
        match self {
            Self::ReadOnly => false,
            Self::PushOnly {
                retire_after_first_successful_push,
            }
            | Self::Mixed {
                retire_after_first_successful_push,
                ..
            } => *retire_after_first_successful_push,
        }
    }

    pub fn pushes(&self) -> bool {
        match self {
            Self::ReadOnly => false,
            Self::PushOnly { .. } | Self::Mixed { .. } => true,
        }
    }
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
        if let LoadModel::Paced {
            read_interval_ms: 0,
        } = self.load_model
        {
            bail!(
                "load_model.read_interval_ms must be greater than 0; use model = \"closed_loop\" for an unpaced saturation run"
            );
        }
        if let OperationMix::Mixed {
            reads_per_push: 0, ..
        } = self.operation_mix
        {
            bail!(
                "operation_mix.reads_per_push must be greater than 0 in mixed mode; use mode = \"push_only\" for a write-only workload"
            );
        }
        if !(0.0..=1.0).contains(&self.canonicalization.sample_rate) {
            bail!("canonicalization.sample_rate must be in [0, 1]");
        }
        if !self.operation_mix.pushes() && self.canonicalization.sample_rate > 0.0 {
            bail!(
                "canonicalization.sample_rate must be 0 when the workload issues no push_delta; canonicalization is only observed after a successful push"
            );
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
