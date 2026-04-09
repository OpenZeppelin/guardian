use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "guardian-prod-benchmarks")]
#[command(about = "Production benchmark runner for GUARDIAN")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Preflight {
        #[arg(long)]
        profile: PathBuf,
    },
    WorkerRun {
        #[arg(long)]
        profile: PathBuf,
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        shard_index: u32,
        #[arg(long)]
        shard_count: u32,
    },
    Aggregate {
        #[arg(long)]
        profile: PathBuf,
        #[arg(long)]
        run_id: String,
        #[arg(long = "worker-artifact")]
        worker_artifacts: Vec<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_cleanup: bool,
    },
    Cleanup {
        #[arg(long)]
        manifest: PathBuf,
    },
}
