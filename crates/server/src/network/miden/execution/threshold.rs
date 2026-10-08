//! The threshold a proposal must meet, read from account state the way the Rust SDK reads it:
//! the override of the one procedure its type invokes, else the account default.

use miden_protocol::Word;
use miden_protocol::account::{Account, StorageMapKey};
use miden_standards::account::auth::AuthGuardedMultisig;
use miden_standards::account::wallets::BasicWallet;

/// The account procedure a proposal type invokes. Types Guardian does not model invoke no
/// known procedure and are held to the account default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvokedProcedure {
    UpdateSigners,
    SetProcedureThreshold,
    UpdateGuardian,
    SendAsset,
    ReceiveAsset,
    AccountDefault,
}

impl InvokedProcedure {
    pub fn of_proposal_type(proposal_type: Option<&str>) -> Self {
        match proposal_type {
            Some("add_signer" | "remove_signer" | "change_threshold") => Self::UpdateSigners,
            Some("update_procedure_threshold") => Self::SetProcedureThreshold,
            Some("switch_guardian") => Self::UpdateGuardian,
            Some("p2id") => Self::SendAsset,
            Some("consume_notes") => Self::ReceiveAsset,
            Some(_) | None => Self::AccountDefault,
        }
    }

    /// Whether the auth procedure needs GUARDIAN's acknowledgment: a guardian switch is
    /// authorized by the cosigners alone.
    pub fn requires_guardian_ack(self) -> bool {
        !matches!(self, Self::UpdateGuardian)
    }

    fn root(self) -> Option<Word> {
        let auth_root = |name: &str| {
            let code = AuthGuardedMultisig::code();
            let export = code
                .exports()
                .find(|export| export.path.to_string().rsplit("::").next() == Some(name))?;
            code.get_procedure_root_by_path(&*export.path)
                .map(Word::from)
        };
        match self {
            Self::UpdateSigners => auth_root("update_signers_and_threshold"),
            Self::SetProcedureThreshold => auth_root("set_procedure_threshold"),
            Self::UpdateGuardian => auth_root("update_guardian_public_key"),
            Self::SendAsset => Some(BasicWallet::move_asset_to_note_root().into()),
            Self::ReceiveAsset => Some(BasicWallet::receive_asset_root().into()),
            Self::AccountDefault => None,
        }
    }
}

/// The effective threshold, or `None` when the account carries no multisig threshold at all.
pub fn effective_threshold(account: &Account, procedure: InvokedProcedure) -> Option<usize> {
    let storage = account.storage();
    let default = storage
        .get_item(AuthGuardedMultisig::threshold_config_slot())
        .ok()?[0]
        .as_canonical_u64() as usize;
    let override_threshold = procedure
        .root()
        .and_then(|root| {
            storage
                .get_map_item(
                    AuthGuardedMultisig::procedure_thresholds_slot(),
                    StorageMapKey::new(root),
                )
                .ok()
        })
        .map(|word| word[0].as_canonical_u64() as usize)
        .filter(|threshold| *threshold != 0);
    Some(override_threshold.unwrap_or(default))
}
