use anyhow::{Context, anyhow};
use miden_protocol::account::AccountId;

use crate::manifest::NetworkName;

use super::{fees, lock::TreasuryLock, network, transfer, treasury::Treasury, usability};

/// What this process has transferred out of the treasury, so a run can report
/// its own spend instead of asserting that funding was not required.
static SPENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The per-run cap, in the chain's fee asset, from `QUAL_SPEND_CAP`.
///
/// Absent means uncapped, which is what a local one-off wants. A value that is
/// present but unparseable is an error rather than uncapped: someone set it
/// meaning to bound the run, and silently ignoring a typo would remove the
/// bound exactly when it was asked for.
fn spend_cap() -> anyhow::Result<Option<u64>> {
    let Ok(raw) = std::env::var("QUAL_SPEND_CAP") else {
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    raw.parse::<u64>()
        .map(Some)
        .map_err(|_| anyhow!("QUAL_SPEND_CAP is set to `{raw}`, which is not a number of units"))
}

/// Where the run's spending is tallied, beside the treasury lock.
///
/// On disk rather than in memory because a run is not one process. The
/// TypeScript leg funds by spawning `qualification-driver fund` once per
/// account, so a counter held in a static would reset on every transfer and cap
/// nothing. The tally is read and written under the treasury lock the caller
/// already holds, which is the same lock that serialises the transfers.
///
/// Keyed by `QUAL_RUN_ID` as well, which every process in a run inherits. The
/// lock is taken per transfer, not for the whole run, so a tally shared by two
/// concurrent runs was capped jointly and zeroed by whichever of them started
/// second.
fn ledger_path(data_dir: &std::path::Path, network: NetworkName) -> std::path::PathBuf {
    ledger_path_for(
        data_dir,
        network,
        std::env::var("QUAL_RUN_ID").ok().as_deref(),
    )
}

fn ledger_path_for(
    data_dir: &std::path::Path,
    network: NetworkName,
    run_id: Option<&str>,
) -> std::path::PathBuf {
    let run = run_id
        .map(|id| {
            id.chars()
                .filter(|character| character.is_ascii_alphanumeric() || "-_.".contains(*character))
                .collect::<String>()
        })
        .filter(|id| !id.is_empty());
    match run {
        Some(run) => data_dir.join(format!("{}.{run}.spent", network.as_str())),
        None => data_dir.join(format!("{}.spent", network.as_str())),
    }
}

fn read_ledger(path: &std::path::Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// Reserves `amount` against the run's cap before anything is transferred.
///
/// A cap checked after the transfer is not a cap.
fn reserve(data_dir: &std::path::Path, network: NetworkName, amount: u64) -> anyhow::Result<()> {
    let path = ledger_path(data_dir, network);
    let already = read_ledger(&path);
    let would_total = already.saturating_add(amount);

    if let Some(cap) = spend_cap()?
        && would_total > cap
    {
        anyhow::bail!(
            "this run would spend {would_total} but QUAL_SPEND_CAP is {cap}; raise the cap \
             deliberately rather than letting an unattended run drain the treasury"
        );
    }

    std::fs::write(&path, would_total.to_string())
        .with_context(|| format!("recording spend in {}", path.display()))?;
    SPENT.store(would_total, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// Clears the tally, so a new run starts from zero rather than inheriting the
/// last one's spending through a reused data directory.
pub fn reset_spend_ledger(data_dir: &std::path::Path, network: NetworkName) {
    let _ = std::fs::remove_file(ledger_path(data_dir, network));
}

pub fn spent_so_far() -> u64 {
    SPENT.load(std::sync::atomic::Ordering::Relaxed)
}

pub struct Funded {
    pub amount: u64,
    pub faucet: AccountId,
    /// Where the funds came from, and so a real counterparty to send to.
    pub treasury: AccountId,
}

/// Funds one account from the treasury, holding the treasury lock for the whole
/// operation.
///
/// Both the command line and the live scenarios come through here, so there is
/// one place that decides what a funded account is and one place that takes the
/// lock.
pub async fn fund_once(
    network: NetworkName,
    data_dir: &std::path::Path,
    recipient: AccountId,
    amount: u64,
) -> anyhow::Result<Option<Funded>> {
    let _lock = TreasuryLock::acquire(data_dir, network.as_str())?;
    let treasury = Treasury::from_env(network)?;
    let mut client = network::connect(network, data_dir).await?;
    network::track_account(
        &mut client,
        data_dir,
        &treasury.account,
        treasury.secret_key(),
    )
    .await?;

    // Observe first: the fee parameters are read from the local view of the
    // chain tip, which is empty until the client has synced at least once.
    let view = network::observe(&mut client, treasury.id()).await?;

    let fees = fees::observe(&client).await?;
    if !fees.charges_fees() {
        return Ok(None);
    }
    let assessment = usability::assess(view.account.as_ref(), &fees, amount, amount);
    if let Some(remediation) = assessment.remediation() {
        return Err(anyhow!("{remediation}"));
    }

    reserve(data_dir, network, amount)?;
    transfer::send(&mut client, treasury.id(), recipient, fees.faucet, amount).await?;
    Ok(Some(Funded {
        amount,
        faucet: fees.faucet,
        treasury: treasury.id(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lock and the ledger only serialize and cap a run while every spender
    /// in it names the same directory. They once did not: the Rust scenarios
    /// passed their per-run account directory and the TypeScript leg's `fund`
    /// subprocess used the treasury default, so each leg took its own lock and
    /// kept its own tally, and the per-run reset cleared a file the Rust leg
    /// never wrote. The tally only grew, and a live run eventually failed its
    /// own cap with nothing wrong.
    #[test]
    fn the_lock_and_the_ledger_share_one_directory() {
        let dir = std::path::Path::new(crate::funding::DEFAULT_TREASURY_DIR);
        assert_eq!(
            ledger_path_for(dir, NetworkName::Testnet, Some("qual-1")).parent(),
            Some(dir),
            "the ledger must sit in the treasury directory, not beside a run's accounts"
        );
    }

    #[test]
    fn the_ledger_is_scoped_per_network() {
        let dir = std::path::Path::new("/tmp/probe");
        assert_ne!(
            ledger_path_for(dir, NetworkName::Testnet, None),
            ledger_path_for(dir, NetworkName::Devnet, None)
        );
    }

    /// Two local runs against one treasury each reset and cap their own tally.
    #[test]
    fn the_ledger_is_scoped_per_run() {
        let dir = std::path::Path::new("/tmp/probe");
        assert_ne!(
            ledger_path_for(dir, NetworkName::Testnet, Some("qual-a")),
            ledger_path_for(dir, NetworkName::Testnet, Some("qual-b"))
        );
        assert_eq!(
            ledger_path_for(dir, NetworkName::Testnet, Some("../qual-a")),
            dir.join("testnet...qual-a.spent"),
            "a run id cannot steer the ledger out of the treasury directory"
        );
    }
}
