//! How a proposal's request is created: executed by the approvers themselves, or stored with the
//! proposal so Guardian may execute it.

use std::num::{NonZeroU16, NonZeroU32};

use guardian_shared::request_envelope::TransactionRequestEnvelope;
use miden_client::Serializable;
use miden_client::transaction::{TransactionRequest, TransactionSummary};

/// The approval window a Guardian-executable proposal gets when the caller sets none: about a
/// day at three-second blocks.
pub const GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA: NonZeroU32 =
    NonZeroU32::new(28_800).expect("non-zero");

/// The transaction expiration a Guardian-executable request scripts, measured from the block
/// it executes against. It keeps a submitted transaction's outcome observable within Guardian's
/// resolution horizon.
pub const GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA: NonZeroU16 =
    NonZeroU16::new(256).expect("non-zero");

/// The `miden-client` version whose serialization a stored request uses. Request bytes carry no
/// version tag of their own, so the server admits them by this name.
pub use guardian_shared::request_envelope::REQUEST_SERIALIZER_ID;

/// Whether proposals this client creates can be executed by Guardian. Set once, on the client.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProposalExecutionMode {
    /// Nothing extra is stored; only a party that can build and prove transactions executes.
    #[default]
    SelfExecuted,
    /// The request is stored with the proposal, bounded so Guardian can execute it.
    GuardianExecutable,
}

impl ProposalExecutionMode {
    /// The approval expiration a new proposal applies: the caller's, else this mode's default.
    pub fn approval_expiration_delta(self, requested: Option<NonZeroU32>) -> Option<NonZeroU32> {
        match self {
            ProposalExecutionMode::SelfExecuted => requested,
            ProposalExecutionMode::GuardianExecutable => {
                Some(requested.unwrap_or(GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA))
            }
        }
    }

    /// The transaction expiration a new proposal's request scripts.
    pub fn transaction_expiration_delta(self) -> Option<NonZeroU16> {
        match self {
            ProposalExecutionMode::SelfExecuted => None,
            ProposalExecutionMode::GuardianExecutable => {
                Some(GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA)
            }
        }
    }

    /// The envelope a new proposal stores its request in, if this mode stores one.
    pub fn attachment(self, request: &TransactionRequest) -> Option<TransactionRequestEnvelope> {
        self.attachment_of_bytes(&request.to_bytes())
    }

    /// [`Self::attachment`] for request bytes a producer serialized, stored exactly as given.
    pub fn attachment_of_bytes(self, request_bytes: &[u8]) -> Option<TransactionRequestEnvelope> {
        match self {
            ProposalExecutionMode::SelfExecuted => None,
            ProposalExecutionMode::GuardianExecutable => Some(TransactionRequestEnvelope::seal(
                request_bytes,
                REQUEST_SERIALIZER_ID,
            )),
        }
    }
}

/// The transaction expiration a signed summary binds, which a rebuilt request must script
/// again to reproduce it.
pub fn summary_expiration_delta(summary: &TransactionSummary) -> Option<NonZeroU16> {
    NonZeroU16::new(summary.expiration_delta())
}

/// The script lines that apply `delta`, placed at the start of a transaction script's `main`.
pub(crate) fn expiration_instructions(delta: Option<NonZeroU16>) -> String {
    match delta {
        Some(delta) => format!(
            "push.{delta}\n            exec.::miden::protocol::tx::update_expiration_block_delta\n            "
        ),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_self_executed_client_applies_no_bound_it_was_not_asked_for() {
        let mode = ProposalExecutionMode::SelfExecuted;
        assert_eq!(mode.approval_expiration_delta(None), None);
        assert_eq!(mode.transaction_expiration_delta(), None);
    }

    #[test]
    fn a_guardian_executable_client_defaults_both_bounds_and_keeps_an_explicit_approval() {
        let mode = ProposalExecutionMode::GuardianExecutable;
        assert_eq!(
            mode.approval_expiration_delta(None),
            Some(GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA)
        );
        let explicit = NonZeroU32::new(500).unwrap();
        assert_eq!(
            mode.approval_expiration_delta(Some(explicit)),
            Some(explicit)
        );
        assert_eq!(
            mode.transaction_expiration_delta(),
            Some(GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA)
        );
    }

    /// The vectors `packages/miden-multisig-client/tests/browser/execution-parity.spec.ts` checks
    /// the TypeScript SDK against: the Guardian-executable auth arguments for fixed inputs and the
    /// roots of the three Guardian-owned scripts with the transaction expiration applied.
    #[test]
    fn guardian_executable_vectors_the_typescript_sdk_must_reproduce() {
        use guardian_shared::SignatureScheme;
        use miden_protocol::Word;
        use miden_protocol::block::BlockNumber;
        use miden_protocol::crypto::SequentialCommit;

        use super::super::configuration::{
            build_update_procedure_threshold_script, build_update_signers_script,
        };
        use super::super::guardian::build_update_guardian_script;
        use crate::procedures::ProcedureName;

        let guardian =
            Word::parse("0xc35d79423c41d46b5289aafef48be2364e9ea494c6b14d6aefad10f1a46e6d7c")
                .unwrap();
        let salt =
            Word::parse("0x0101010101010101020202020202020203030303030303030404040404040404")
                .unwrap();
        let delta = Some(GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA);
        let fee_faucet = miden_client::testing::mock::MockRpcApi::new(
            miden_client::testing::MockChain::builder().build().unwrap(),
        )
        .protocol_config()
        .fee_asset_id()
        .faucet_id();
        let auth_args = crate::transaction::multisig_auth_args(
            fee_faucet,
            BlockNumber::from(1),
            salt,
            Some(GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA),
        )
        .unwrap();
        let vectors = [
            ("authArg", auth_args.to_commitment().to_hex()),
            (
                "updateSigners",
                build_update_signers_script(delta).unwrap().root().to_hex(),
            ),
            (
                "updateProcedureThreshold",
                build_update_procedure_threshold_script(ProcedureName::SendAsset, 2, delta)
                    .unwrap()
                    .root()
                    .to_hex(),
            ),
            (
                "updateGuardian",
                build_update_guardian_script(guardian, SignatureScheme::Falcon, delta)
                    .unwrap()
                    .root()
                    .to_hex(),
            ),
        ];
        assert_eq!(
            vectors,
            [
                (
                    "authArg",
                    "0xec66ad44aba543365dc7d859d9e4b09cbd84bfdfb76fbb388b994eeafc0546b8"
                        .to_string()
                ),
                (
                    "updateSigners",
                    "0x10812c997e2896a5be3684de226024527df5fcdb6d6a9e2ba76032e5ca9a2c56"
                        .to_string()
                ),
                (
                    "updateProcedureThreshold",
                    "0x62ca1dd0ed90a2747a28abc917068dcb0b58dcb1f355460242e35fe2227f72ca"
                        .to_string()
                ),
                (
                    "updateGuardian",
                    "0x9aadf7d81015fe48295b290aef188da918b99c748b5db1a632081f85f6e446e5"
                        .to_string()
                ),
            ]
        );
        assert_ne!(
            build_update_signers_script(None).unwrap().root().to_hex(),
            vectors[1].1,
            "the expiration line changes the script, so the vector pins it"
        );
    }
}
