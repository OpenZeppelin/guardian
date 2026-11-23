pub use guardian_shared::{FromJson, ToJson};

use server::builder::ServerBuilder;

#[cfg(feature = "postgres")]
use server::builder::handle::StartupRecoveryError;
use server::builder::handle::StartupRecoveryMode;
use std::env;

const STARTUP_RECOVERY_ENV: &str = "GUARDIAN_STARTUP_ACCOUNT_RECOVERY";

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let startup_recovery = match env::var(STARTUP_RECOVERY_ENV) {
        Ok(value) => value
            .parse::<StartupRecoveryMode>()
            .unwrap_or_else(|error| {
                eprintln!("{STARTUP_RECOVERY_ENV} {error}");
                std::process::exit(2);
            }),
        Err(env::VarError::NotPresent) => StartupRecoveryMode::Off,
        Err(env::VarError::NotUnicode(_)) => {
            eprintln!("{STARTUP_RECOVERY_ENV} must be valid UTF-8");
            std::process::exit(2);
        }
    };
    #[cfg(not(feature = "postgres"))]
    if startup_recovery != StartupRecoveryMode::Off {
        eprintln!("Account recovery requires a Postgres build");
        std::process::exit(2);
    }

    let server = ServerBuilder::from_env()
        .await
        .unwrap_or_else(|error| panic!("Failed to configure server: {error}"))
        .build()
        .await
        .unwrap_or_else(|error| panic!("Failed to build server: {error}"));

    #[cfg(feature = "postgres")]
    {
        match server.recover_accounts_on_startup(startup_recovery).await {
            Ok(()) => {}
            Err(error @ StartupRecoveryError::Incomplete(_)) => {
                tracing::error!(%error, "Startup account recovery did not finish; serving without it");
            }
            Err(
                error @ (StartupRecoveryError::LeaseBusy
                | StartupRecoveryError::LeaseUnavailable(_)),
            ) => {
                tracing::error!(%error, "Cannot serve while account recovery may be active elsewhere");
                std::process::exit(1);
            }
        }
    }

    server.run().await;
}
