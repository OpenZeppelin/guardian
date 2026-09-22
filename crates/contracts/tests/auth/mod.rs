pub mod fee_payment;
pub mod multisig;

use miden_protocol::Word;
use miden_protocol::crypto::SequentialCommit;
use miden_standards::account::auth::MultisigAuthArgs;
use miden_testing::{MockChain, MockTransactionBuilder};

/// The 0.17 multisig auth args for `salt`, bound to `mock_chain`'s tip: the
/// reference block of a transaction built now. The approval never expires and
/// no fee conversion info is committed: `MockChain` charges no fee and the
/// guarded component pays none on this protocol line.
pub fn auth_args_at_tip(mock_chain: &MockChain, salt: Word) -> MultisigAuthArgs {
    MultisigAuthArgs::new(mock_chain.latest_block_header().block_num(), salt)
}

/// Attaches multisig auth args to a mock transaction the way the SDKs do.
pub trait MultisigAuthArgsExt {
    /// Sets the commitment as the auth arg, puts the preimage in the advice
    /// map, and carries the bound block so the kernel can read its commitment
    /// when it is older than the reference block (`required_block` ignores a
    /// block at or after the reference block).
    fn multisig_auth_args(self, auth_args: &MultisigAuthArgs) -> Self;
}

impl MultisigAuthArgsExt for MockTransactionBuilder<'_> {
    fn multisig_auth_args(self, auth_args: &MultisigAuthArgs) -> Self {
        let commitment = auth_args.to_commitment();
        self.add_advice_map_entry(commitment, auth_args.to_elements())
            .auth_args(commitment)
            .required_block(auth_args.bound_block_num())
    }
}
