use anyhow::{Result, bail};
use clap::{Parser, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
pub enum Scenario {
    StateRead,
    StateWrite,
    Mixed,
    StateSync,
    Canonicalization,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
pub enum AuthScheme {
    Falcon,
    Ecdsa,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
pub enum Transport {
    Grpc,
    Http,
}

#[derive(Clone, Debug, Parser, Serialize)]
#[command(name = "psm-server-bench-loadgen")]
#[command(about = "Benchmark driver for PSM server")]
pub struct RunConfig {
    #[arg(long, default_value = "http://localhost:50051")]
    pub psm_endpoint: String,

    #[arg(long, default_value = "http://localhost:3000")]
    pub psm_http_endpoint: String,

    #[arg(long, value_enum, default_value_t = Transport::Grpc)]
    pub transport: Transport,

    #[arg(long, default_value_t = 8)]
    pub users: u32,

    #[arg(long, default_value_t = 32)]
    pub accounts: u32,

    #[arg(long, default_value_t = 2)]
    pub signers_per_account: u32,

    #[arg(long, value_enum, default_value_t = AuthScheme::Falcon)]
    pub auth_scheme: AuthScheme,

    #[arg(long, default_value_t = 128)]
    pub ops_per_user: u32,

    #[arg(long, value_enum, default_value_t = Scenario::StateRead)]
    pub scenario: Scenario,

    #[arg(long, default_value_t = 30)]
    pub mixed_write_percent: u32,

    #[arg(long, default_value_t = 4)]
    pub state_sync_reads_per_push: u32,

    #[arg(long, default_value_t = 250)]
    pub canonicalization_poll_interval_ms: u64,

    #[arg(long, default_value_t = 90)]
    pub canonicalization_timeout_secs: u64,

    #[arg(long)]
    pub output: Option<PathBuf>,
}

impl RunConfig {
    pub fn validate(&self) -> Result<()> {
        if self.users == 0 {
            bail!("users must be greater than 0");
        }
        if self.accounts == 0 {
            bail!("accounts must be greater than 0");
        }
        if self.accounts < self.users {
            bail!("accounts must be greater than or equal to users");
        }
        if self.signers_per_account == 0 {
            bail!("signers-per-account must be greater than 0");
        }
        if self.ops_per_user == 0 {
            bail!("ops-per-user must be greater than 0");
        }
        if self.mixed_write_percent > 100 {
            bail!("mixed-write-percent must be in range [0, 100]");
        }
        if self.state_sync_reads_per_push == 0 {
            bail!("state-sync-reads-per-push must be greater than 0");
        }
        if self.canonicalization_poll_interval_ms == 0 {
            bail!("canonicalization-poll-interval-ms must be greater than 0");
        }
        if self.canonicalization_timeout_secs == 0 {
            bail!("canonicalization-timeout-secs must be greater than 0");
        }
        if self.psm_http_endpoint.trim().is_empty() {
            bail!("psm-http-endpoint must not be empty");
        }
        Ok(())
    }
}
