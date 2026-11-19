//! One-time startup repair of accounts whose stored state and signer set
//! were replaced through an unverified `/configure` (Guardian 0.18.3 and
//! earlier).
//!
//! The on-chain commitment is the only trust anchor. For each account
//! whose stored commitment is not the on-chain commitment, the block
//! rebuilds the account from its creation delta, applies the stored delta
//! rows, and accepts the result only when its commitment equals the
//! on-chain commitment. It then removes the rows that the true signer set
//! did not approve. An account that cannot be proven is paused and
//! recorded in the recovery file.

mod deltas;
pub mod plan;
mod proposals;
mod replay;
pub mod report;
mod rows;
pub mod runner;
pub mod types;
mod unrecovered;

#[cfg(test)]
mod plan_tests;

pub use plan::plan_account;
pub use runner::run_self_heal;
pub use types::{SelfHealConfig, SelfHealMode, SelfHealTotals};
