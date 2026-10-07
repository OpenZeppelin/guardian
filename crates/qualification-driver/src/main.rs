use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand, ValueEnum};
use guardian_qualification_driver::manifest::{Manifest, NetworkName, Profile, Sdk, validate};
use guardian_qualification_driver::report::{ArtifactSet, Pairing, Trigger, merge};
use guardian_qualification_driver::run::{self, RunOptions};
use guardian_qualification_driver::scenario::Endpoints;

#[derive(Parser)]
#[command(name = "qualification-driver", about = "Guardian qualification driver")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Load the scenario manifest and coverage matrix, then validate them.
    Validate {
        #[arg(long, default_value = "qualification/manifest/scenarios.toml")]
        scenarios: PathBuf,
        #[arg(long, default_value = "qualification/manifest/matrix.toml")]
        matrix: PathBuf,
    },
    /// Print the required scenario set for every declared pair.
    Matrix {
        #[arg(long, default_value = "qualification/manifest/scenarios.toml")]
        scenarios: PathBuf,
        #[arg(long, default_value = "qualification/manifest/matrix.toml")]
        matrix: PathBuf,
    },
    /// Emit the validated manifest as JSON for the TypeScript driver.
    ExportManifest {
        #[arg(long, default_value = "qualification/manifest/scenarios.toml")]
        scenarios: PathBuf,
        #[arg(long, default_value = "qualification/manifest/matrix.toml")]
        matrix: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Clear this run's spend tally, so a reused data directory does not carry
    /// the previous run's spending into the cap.
    SpendReset {
        #[arg(long, value_enum)]
        network: NetworkArg,
        #[arg(long, default_value = guardian_qualification_driver::funding::DEFAULT_TREASURY_DIR)]
        data_dir: PathBuf,
    },
    /// Create a treasury key, printing the secret once and the address to fund.
    TreasuryNew {
        #[arg(long, value_enum)]
        network: NetworkArg,
    },
    /// Print the address of the configured treasury, for topping up.
    TreasuryAddress {
        #[arg(long, value_enum)]
        network: NetworkArg,
    },
    /// Report what the network shows for the configured treasury.
    TreasuryStatus {
        #[arg(long, value_enum)]
        network: NetworkArg,
        #[arg(long, default_value = guardian_qualification_driver::funding::DEFAULT_TREASURY_DIR)]
        data_dir: PathBuf,
    },
    /// Consume the notes sent to the treasury, deploying it and funding its vault.
    TreasuryBootstrap {
        #[arg(long, value_enum)]
        network: NetworkArg,
        #[arg(long, default_value = guardian_qualification_driver::funding::DEFAULT_TREASURY_DIR)]
        data_dir: PathBuf,
    },
    /// Fund one account from the treasury.
    ///
    /// Both drivers call this rather than each implementing funding, so there
    /// is one implementation and the treasury key never enters the TypeScript
    /// process.
    Fund {
        #[arg(long, value_enum)]
        network: NetworkArg,
        #[arg(long)]
        recipient: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value = guardian_qualification_driver::funding::DEFAULT_TREASURY_DIR)]
        data_dir: PathBuf,
    },
    /// Run the funding preflight: lock, fee model, usability, and projection.
    TreasuryCheck {
        #[arg(long, value_enum)]
        network: NetworkArg,
        #[arg(long, default_value = guardian_qualification_driver::funding::DEFAULT_TREASURY_DIR)]
        data_dir: PathBuf,
        /// What this run expects to need, in the chain's fee asset.
        #[arg(long, default_value = "100000")]
        required: u64,
        /// What one run is expected to cost, used for the depletion projection.
        #[arg(long, default_value = "100000")]
        per_run_cost: u64,
    },
    /// Move funds out of a superseded private treasury into the public one.
    TreasurySweep {
        #[arg(long, value_enum)]
        network: NetworkArg,
        #[arg(long, default_value = guardian_qualification_driver::funding::DEFAULT_TREASURY_DIR)]
        data_dir: PathBuf,
    },
    /// Print the operator public keys the allowlist must carry.
    OperatorKeys {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    /// Sign one proposal with a handed-over cosigner key.
    ///
    /// The cross-SDK handoff scenarios call this from the other driver, so the
    /// signature genuinely comes from this SDK rather than from a shared
    /// process.
    Cosign {
        #[arg(long, value_enum)]
        network: NetworkArg,
        #[arg(long)]
        guardian_endpoint: String,
        #[arg(long)]
        account_id: String,
        #[arg(long)]
        proposal_id: String,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long, default_value = "/tmp/qualification-handoff")]
        account_dir: PathBuf,
    },
    /// Merge run results from a directory without collapsing networks.
    Report {
        #[arg(long)]
        results: PathBuf,
        /// Merge only this run's files. The results directory is shared across
        /// runs, and a run whose Rust leg died leaves its TypeScript results
        /// behind, so without this an old orphan fails every later merge.
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long, default_value = "qualification/manifest/scenarios.toml")]
        scenarios: PathBuf,
        #[arg(long, default_value = "qualification/manifest/matrix.toml")]
        matrix: PathBuf,
    },
    /// Execute the scenarios for a profile against a provisioned stack.
    Run {
        #[arg(long, value_enum)]
        profile: ProfileArg,
        #[arg(long, value_enum)]
        network: Option<NetworkArg>,
        #[arg(long, value_enum)]
        sdk: Option<SdkArg>,
        #[arg(long)]
        scenario: Vec<String>,
        #[arg(long, default_value = "false")]
        core_only: bool,
        #[arg(long, default_value = "false")]
        filtered: bool,
        #[arg(long, default_value = "false")]
        post_restart: bool,
        #[arg(long)]
        run_id: String,
        #[arg(long, value_enum, default_value = "dispatch")]
        trigger: TriggerArg,
        #[arg(long)]
        requested_by: Option<String>,
        #[arg(long)]
        http_endpoint: String,
        #[arg(long)]
        grpc_endpoint: String,
        #[arg(long, default_value = "")]
        image_digest: String,
        #[arg(long, default_value = "")]
        image_revision: String,
        #[arg(long, value_enum, default_value = "branch")]
        pairing: PairingArg,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "qualification/manifest")]
        manifest_dir: PathBuf,
        #[arg(long, default_value = "/tmp/qualification-accounts")]
        account_dir: PathBuf,
        /// The treasury's own directory, shared by every spender in the run.
        /// Separate from `account_dir`, which is per run.
        #[arg(long, default_value = guardian_qualification_driver::funding::DEFAULT_TREASURY_DIR)]
        treasury_dir: PathBuf,
    },
}

#[derive(Copy, Clone, ValueEnum)]
enum ProfileArg {
    Deterministic,
    Live,
}

#[derive(Copy, Clone, ValueEnum)]
enum SchemeArg {
    Falcon,
    Ecdsa,
}

impl From<SchemeArg> for guardian_qualification_driver::manifest::Scheme {
    fn from(value: SchemeArg) -> Self {
        match value {
            SchemeArg::Falcon => Self::Falcon,
            SchemeArg::Ecdsa => Self::Ecdsa,
        }
    }
}

#[derive(Copy, Clone, ValueEnum)]
enum NetworkArg {
    Devnet,
    Testnet,
}

#[derive(Copy, Clone, ValueEnum)]
enum SdkArg {
    Rust,
    Typescript,
}

#[derive(Copy, Clone, ValueEnum)]
enum TriggerArg {
    Schedule,
    Dispatch,
    PullRequest,
    Publication,
    PreRelease,
}

#[derive(Copy, Clone, ValueEnum)]
enum PairingArg {
    Branch,
    Release,
    Published,
}

impl From<ProfileArg> for Profile {
    fn from(value: ProfileArg) -> Self {
        match value {
            ProfileArg::Deterministic => Self::Deterministic,
            ProfileArg::Live => Self::Live,
        }
    }
}

impl From<NetworkArg> for NetworkName {
    fn from(value: NetworkArg) -> Self {
        match value {
            NetworkArg::Devnet => Self::Devnet,
            NetworkArg::Testnet => Self::Testnet,
        }
    }
}

impl From<SdkArg> for Sdk {
    fn from(value: SdkArg) -> Self {
        match value {
            SdkArg::Rust => Self::Rust,
            SdkArg::Typescript => Self::Typescript,
        }
    }
}

impl From<TriggerArg> for Trigger {
    fn from(value: TriggerArg) -> Self {
        match value {
            TriggerArg::Schedule => Self::Schedule,
            TriggerArg::Dispatch => Self::Dispatch,
            TriggerArg::PullRequest => Self::PullRequest,
            TriggerArg::Publication => Self::Publication,
            TriggerArg::PreRelease => Self::PreRelease,
        }
    }
}

impl From<PairingArg> for Pairing {
    fn from(value: PairingArg) -> Self {
        match value {
            PairingArg::Branch => Self::Branch,
            PairingArg::Release => Self::Release,
            PairingArg::Published => Self::Published,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<std::process::ExitCode> {
    match Cli::parse().command {
        Command::Validate { scenarios, matrix } => {
            let manifest = Manifest::load(&scenarios, &matrix).with_context(|| {
                format!("loading {} and {}", scenarios.display(), matrix.display())
            })?;
            match validate::validate(&manifest) {
                Ok(()) => {
                    println!(
                        "manifest valid: {} scenarios, {} networks, {} pairs",
                        manifest.scenarios.len(),
                        manifest.networks.len(),
                        manifest.pairs.len()
                    );
                    Ok(std::process::ExitCode::SUCCESS)
                }
                Err(errors) => {
                    for error in &errors {
                        eprintln!("error: {error}");
                    }
                    bail!("{} manifest validation error(s)", errors.len())
                }
            }
        }
        Command::Matrix { scenarios, matrix } => {
            let manifest = Manifest::load(&scenarios, &matrix)?;
            for ((network, sdk), ids) in manifest.required_by_pair() {
                println!("{}/{:?}: {} required", network.as_str(), sdk, ids.len());
                for id in ids {
                    println!("  {id}");
                }
            }
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::Cosign {
            network,
            guardian_endpoint,
            account_id,
            proposal_id,
            key_file,
            account_dir,
        } => {
            let network: NetworkName = network.into();
            guardian_qualification_driver::handoff::cosign(
                network,
                guardian_endpoint,
                &account_id,
                &proposal_id,
                &key_file,
                &account_dir,
            )
            .await?;
            println!("signed {proposal_id}");
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::Report {
            results,
            run_id,
            scenarios,
            matrix,
        } => {
            // Loaded, not attempted. Restating the claim over both SDKs' results
            // is the whole reason the merger takes a manifest; swallowing a load
            // failure would leave a Rust-only claim standing beside a folded-in
            // TypeScript failure, which is the defect the restatement exists to
            // prevent.
            let manifest = Manifest::load(&scenarios, &matrix).with_context(|| {
                format!("loading {} and {}", scenarios.display(), matrix.display())
            })?;
            let merged = merge::merge_directory(&results, Some(&manifest), run_id.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&merged)?);
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::SpendReset { network, data_dir } => {
            guardian_qualification_driver::funding::service::reset_spend_ledger(
                &data_dir,
                network.into(),
            );
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::TreasuryNew { network } => {
            let network: NetworkName = network.into();
            let (treasury, secret) =
                guardian_qualification_driver::funding::Treasury::generate(network)?;
            eprintln!("Treasury created for {}.", network.as_str());
            eprintln!();
            eprintln!("  address (fund this from the faucet):");
            eprintln!("    {}", treasury.address());
            eprintln!();
            eprintln!("  account id: {}", treasury.id());
            eprintln!();
            eprintln!("The secret is printed on stdout once and is not stored.");
            eprintln!("Put it in the qualification environment as QUAL_TREASURY_KEY.");
            println!("{secret}");
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::TreasuryAddress { network } => {
            let network: NetworkName = network.into();
            let treasury = guardian_qualification_driver::funding::Treasury::from_env(network)?;
            println!("{}", treasury.address());
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::TreasuryStatus { network, data_dir } => {
            let network: NetworkName = network.into();
            let treasury = guardian_qualification_driver::funding::Treasury::from_env(network)?;
            let mut client =
                guardian_qualification_driver::funding::network::connect(network, &data_dir)
                    .await?;
            guardian_qualification_driver::funding::network::track_account(
                &mut client,
                &data_dir,
                &treasury.account,
                treasury.secret_key(),
            )
            .await?;
            let view = guardian_qualification_driver::funding::network::observe(
                &mut client,
                treasury.id(),
            )
            .await?;

            println!("network      : {}", network.as_str());
            println!("address      : {}", treasury.address());
            println!("account id   : {}", treasury.id());
            println!("chain height : {}", view.block_number);
            match &view.account {
                Some(account) => {
                    let balances =
                        guardian_qualification_driver::funding::network::vault_balances(account);
                    println!("deployed     : yes (nonce {})", account.nonce());
                    if balances.is_empty() {
                        println!("vault        : empty");
                    } else {
                        for (faucet, amount) in balances {
                            println!("vault        : {amount} of {faucet}");
                        }
                    }
                }
                None => println!(
                    "deployed     : no (the account appears on chain only once it transacts)"
                ),
            }
            println!("notes waiting: {}", view.consumable_note_count);
            for (id, status) in &view.transactions {
                println!("transaction  : {id} {status}");
            }
            if view.consumable_note_count > 0 && view.account.is_none() {
                println!();
                println!("Run `treasury-bootstrap` to consume them; that transaction deploys the");
                println!("account and pays its own fee out of the note it consumes.");
            }
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::TreasuryBootstrap { network, data_dir } => {
            let network: NetworkName = network.into();
            let treasury = guardian_qualification_driver::funding::Treasury::from_env(network)?;
            let mut client =
                guardian_qualification_driver::funding::network::connect(network, &data_dir)
                    .await?;
            guardian_qualification_driver::funding::network::track_account(
                &mut client,
                &data_dir,
                &treasury.account,
                treasury.secret_key(),
            )
            .await?;
            let consumed =
                guardian_qualification_driver::funding::bootstrap::consume_pending_notes(
                    &mut client,
                    treasury.id(),
                )
                .await?;
            if consumed == 0 {
                println!("no notes were waiting for {}", treasury.address());
            } else {
                println!("consumed {consumed} note(s) for {}", treasury.address());
            }
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::Fund {
            network,
            recipient,
            amount,
            data_dir,
        } => {
            use guardian_qualification_driver::funding;

            let network: NetworkName = network.into();
            let recipient =
                miden_protocol::account::AccountId::from_hex(&recipient).map_err(|error| {
                    anyhow::anyhow!("recipient `{recipient}` is malformed: {error}")
                })?;

            // Through the shared path, not a copy of it. The copy that used to
            // live here skipped the spend counter, and the TypeScript leg funds
            // by shelling out to this command, so every TypeScript run
            // under-reported what it moved. It also had no cap.
            match funding::service::fund_once(network, &data_dir, recipient, amount).await {
                Ok(Some(funded)) => {
                    println!(
                        "{{\"funded\":\"{recipient}\",\"amount\":{},\"faucet\":\"{}\",\"treasury\":\"{}\"}}",
                        funded.amount, funded.faucet, funded.treasury
                    );
                    Ok(std::process::ExitCode::SUCCESS)
                }
                Ok(None) => {
                    println!("this chain charges nothing; no funding was needed");
                    Ok(std::process::ExitCode::SUCCESS)
                }
                Err(error) => {
                    eprintln!("{error}");
                    Ok(std::process::ExitCode::from(2))
                }
            }
        }
        Command::TreasuryCheck {
            network,
            data_dir,
            required,
            per_run_cost,
        } => {
            use guardian_qualification_driver::funding;

            let network: NetworkName = network.into();
            let _lock = funding::lock::TreasuryLock::acquire(&data_dir, network.as_str())?;
            let treasury = funding::Treasury::from_env(network)?;
            let mut client = funding::network::connect(network, &data_dir).await?;
            funding::network::track_account(
                &mut client,
                &data_dir,
                &treasury.account,
                treasury.secret_key(),
            )
            .await?;

            let view = funding::network::observe(&mut client, treasury.id()).await?;
            let fees = funding::fees::observe(&client).await?;
            let usability =
                funding::usability::assess(view.account.as_ref(), &fees, required, per_run_cost);
            let budget = funding::budget::SpendBudget::new(required);
            let summary = funding::summary::summarize(&usability, &budget);

            println!("network   : {}", network.as_str());
            println!("address   : {}", treasury.address());
            println!(
                "fee model : faucet {} base fee {}{}",
                fees.faucet,
                fees.verification_base_fee,
                if fees.charges_fees() {
                    ""
                } else {
                    " (this chain charges nothing)"
                }
            );
            println!("usability : {usability:?}");
            println!("summary   : {summary:?}");

            match usability.remediation() {
                Some(remediation) => {
                    eprintln!();
                    eprintln!("{remediation}");
                    Ok(std::process::ExitCode::from(2))
                }
                None => Ok(std::process::ExitCode::SUCCESS),
            }
        }
        Command::TreasurySweep { network, data_dir } => {
            let network: NetworkName = network.into();
            // Under the same lock every other treasury transaction takes. A
            // sweep moves the treasury's own funds, so running it beside a
            // funding transfer would build both on the same nonce.
            let _lock = guardian_qualification_driver::funding::lock::TreasuryLock::acquire(
                &data_dir,
                network.as_str(),
            )?;
            let secret = std::env::var("QUAL_TREASURY_KEY")
                .map_err(|_| anyhow::anyhow!("QUAL_TREASURY_KEY is not set"))?;
            let legacy =
                guardian_qualification_driver::funding::Treasury::legacy_private(&secret, network)?;
            let current = guardian_qualification_driver::funding::Treasury::from_secret_hex(
                &secret, network,
            )?;

            let mut client =
                guardian_qualification_driver::funding::network::connect(network, &data_dir)
                    .await?;
            guardian_qualification_driver::funding::network::track_account(
                &mut client,
                &data_dir,
                &legacy.account,
                legacy.secret_key(),
            )
            .await?;
            guardian_qualification_driver::funding::network::track_account(
                &mut client,
                &data_dir,
                &current.account,
                current.secret_key(),
            )
            .await?;

            let view =
                guardian_qualification_driver::funding::network::observe(&mut client, legacy.id())
                    .await?;
            let Some(account) = view.account else {
                anyhow::bail!(
                    "the superseded treasury is not tracked locally, so it cannot be swept"
                );
            };
            let balances =
                guardian_qualification_driver::funding::network::vault_balances(&account);
            let Some((faucet, amount)) = balances.first().copied() else {
                println!("the superseded treasury holds nothing");
                return Ok(std::process::ExitCode::SUCCESS);
            };

            // Leave a margin so the sweep can pay its own fee.
            let margin = amount / 100;
            let sending = amount.saturating_sub(margin.max(1_000));
            println!("sweeping {sending} of {faucet} to {}", current.address());

            guardian_qualification_driver::funding::transfer::send(
                &mut client,
                legacy.id(),
                current.id(),
                faucet,
                sending,
            )
            .await?;
            println!("sent; the recipient must consume the note to receive it");
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::OperatorKeys { repo_root } => {
            let (reader, restricted) =
                guardian_qualification_driver::fixtures::operator_public_keys(&repo_root)?;
            println!(
                "{}",
                serde_json::json!({ "reader": reader, "restricted": restricted })
            );
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::ExportManifest {
            scenarios,
            matrix,
            out,
        } => {
            let manifest = Manifest::load(&scenarios, &matrix)?;
            if let Err(errors) = validate::validate(&manifest) {
                for error in &errors {
                    eprintln!("error: {error}");
                }
                bail!("refusing to export an invalid manifest");
            }
            let rendered = manifest.to_export_json()?;
            match out {
                Some(path) => {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&path, rendered)?;
                    println!("wrote {}", path.display());
                }
                None => println!("{rendered}"),
            }
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::Run {
            profile,
            network,
            sdk,
            scenario,
            core_only,
            filtered,
            post_restart,
            run_id,
            trigger,
            requested_by,
            http_endpoint,
            grpc_endpoint,
            image_digest,
            image_revision,
            pairing,
            out,
            manifest_dir,
            account_dir,
            treasury_dir,
        } => {
            let options = RunOptions {
                profile: profile.into(),
                network: network.map(Into::into),
                sdk: sdk.map(Into::into),
                scenarios: scenario,
                core_only,
                filtered,
                post_restart,
                run_id,
                trigger: trigger.into(),
                requested_by,
                endpoints: Endpoints {
                    http: http_endpoint,
                    grpc: grpc_endpoint,
                },
                artifact_set: ArtifactSet {
                    image_digest,
                    image_revision,
                    pairing: pairing.into(),
                    sdk_versions: Default::default(),
                    sdk_integrity: Default::default(),
                    miden_versions: Default::default(),
                },
                out,
                account_dir,
                treasury_dir,
            };
            let (result, code) = run::execute(&manifest_dir, options).await?;
            println!(
                "conclusion={:?} claim={:?} scenarios={}",
                result.conclusion,
                result.qualification_claim,
                result.scenario_results.len()
            );
            Ok(std::process::ExitCode::from(code as u8))
        }
    }
}
