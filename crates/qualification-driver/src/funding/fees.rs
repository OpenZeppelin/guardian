use anyhow::anyhow;
use miden_protocol::account::AccountId;

use super::network::MidenClient;

/// What the chain charges, read from the tip rather than assumed.
///
/// A zero-fee chain needs no funding at all, and asserting a funding step there
/// would be asserting something that cannot happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeModel {
    pub faucet: AccountId,
    pub verification_base_fee: u32,
}

impl FeeModel {
    pub fn charges_fees(&self) -> bool {
        self.verification_base_fee > 0
    }
}

pub async fn observe(client: &MidenClient) -> anyhow::Result<FeeModel> {
    let header = client
        .get_latest_block_header()
        .await
        .map_err(|error| anyhow!("cannot read the chain tip: {error}"))?;
    let parameters = header.fee_parameters();
    Ok(FeeModel {
        faucet: parameters.fee_faucet_id(),
        verification_base_fee: parameters.verification_base_fee(),
    })
}
