// E2E tests (enabled with `--features e2e`)
#![cfg(feature = "e2e")]

mod abandon_candidate;
mod configure_account;
mod switch_guardian_canonicalization;

use miden_protocol::crypto::SequentialCommit;
use miden_standards::account::auth::MultisigAuthArgs;
use miden_testing::MockTransactionBuilder;

/// Attaches multisig auth args to a mock transaction the way the SDKs do: the
/// commitment as the auth arg and the three-word preimage in the advice map.
/// The SDKs also declare the bound block on the request so it executes at a
/// later tip; a mock transaction here executes at the block it binds, which
/// needs no declaration.
trait MultisigAuthArgsExt {
    fn multisig_auth_args(self, auth_args: &MultisigAuthArgs) -> Self;
}

impl MultisigAuthArgsExt for MockTransactionBuilder<'_> {
    fn multisig_auth_args(self, auth_args: &MultisigAuthArgs) -> Self {
        let commitment = auth_args.to_commitment();
        self.add_advice_map_entry(commitment, auth_args.to_elements())
            .auth_args(commitment)
    }
}
