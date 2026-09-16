use anyhow::anyhow;
use miden_protocol::account::AccountId;

use crate::manifest::NetworkName;

use super::{fees, lock::TreasuryLock, network, transfer, treasury::Treasury, usability};

/// What this process has transferred out of the treasury, so a run can report
/// its own spend instead of asserting that funding was not required.
static SPENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

    transfer::send(&mut client, treasury.id(), recipient, fees.faucet, amount).await?;
    SPENT.fetch_add(amount, std::sync::atomic::Ordering::Relaxed);
    Ok(Some(Funded {
        amount,
        faucet: fees.faucet,
        treasury: treasury.id(),
    }))
}
