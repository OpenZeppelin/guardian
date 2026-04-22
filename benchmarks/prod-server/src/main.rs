use anyhow::Result;
use clap::Parser;
use guardian_prod_benchmarks::cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Preflight { profile } => guardian_prod_benchmarks::preflight::run(&profile).await,
        Commands::WorkerRun {
            profile,
            run_id,
            shard_index,
            shard_count,
        } => {
            guardian_prod_benchmarks::run::run_worker(&profile, run_id, shard_index, shard_count)
                .await
        }
        Commands::Aggregate {
            profile,
            run_id,
            worker_artifacts,
            no_cleanup,
        } => {
            guardian_prod_benchmarks::run::aggregate(
                &profile,
                &run_id,
                &worker_artifacts,
                no_cleanup,
            )
            .await
        }
        Commands::Cleanup { manifest } => guardian_prod_benchmarks::cleanup::run(&manifest).await,
    }
}
