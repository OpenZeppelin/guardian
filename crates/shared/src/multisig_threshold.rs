//! The signature threshold a guarded-multisig transaction must meet, read from account state.
//!
//! On chain (`multisig.masm` `compute_transaction_threshold`) every account procedure a
//! transaction calls contributes its per-procedure override, else the account default, and the
//! highest of them applies. This module is the one Rust statement of which procedures each
//! proposal type calls: GUARDIAN's executor and the Rust SDK both read it, and the TypeScript
//! SDK is held to it by the shared threshold vectors.

use miden_protocol::Word;
use miden_protocol::account::{Account, StorageMapKey};
use miden_standards::account::auth::AuthGuardedMultisig;
use miden_standards::account::wallets::BasicWallet;

/// An account procedure a proposal type calls whose threshold can be overridden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThresholdProcedure {
    UpdateSigners,
    SetProcedureThreshold,
    UpdateGuardian,
    SendAsset,
    CreateNote,
    ReceiveAsset,
}

impl ThresholdProcedure {
    /// The procedure's MAST root, the key of its override in the procedure thresholds map.
    pub fn root(self) -> Word {
        match self {
            Self::UpdateSigners => auth_root("update_signers_and_threshold"),
            Self::SetProcedureThreshold => auth_root("set_procedure_threshold"),
            Self::UpdateGuardian => auth_root("update_guardian_public_key"),
            Self::SendAsset => BasicWallet::move_asset_to_note_root().into(),
            Self::CreateNote => BasicWallet::create_note_root().into(),
            Self::ReceiveAsset => BasicWallet::receive_asset_root().into(),
        }
    }
}

fn auth_root(name: &str) -> Word {
    let code = AuthGuardedMultisig::code();
    code.exports()
        .find(|export| export.path.to_string().rsplit("::").next() == Some(name))
        .and_then(|export| code.get_procedure_root_by_path(&*export.path))
        .map(Word::from)
        .unwrap_or_else(|| panic!("AuthGuardedMultisig exports {name}"))
}

/// What a proposal does, as far as its threshold goes. Types the protocol does not model call
/// no known procedure and are held to the account default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThresholdedTransaction {
    UpdateSigners,
    SetProcedureThreshold,
    UpdateGuardian,
    P2id,
    ConsumeNotes,
    AccountDefault,
}

impl ThresholdedTransaction {
    pub fn of_proposal_type(proposal_type: Option<&str>) -> Self {
        match proposal_type {
            Some("add_signer" | "remove_signer" | "change_threshold") => Self::UpdateSigners,
            Some("update_procedure_threshold") => Self::SetProcedureThreshold,
            Some("switch_guardian") => Self::UpdateGuardian,
            Some("p2id") => Self::P2id,
            Some("consume_notes") => Self::ConsumeNotes,
            Some(_) | None => Self::AccountDefault,
        }
    }

    /// The overridable procedures the transaction calls. A P2ID transfer calls both
    /// `move_asset_to_note` and `create_note`.
    pub fn procedures(self) -> &'static [ThresholdProcedure] {
        match self {
            Self::UpdateSigners => &[ThresholdProcedure::UpdateSigners],
            Self::SetProcedureThreshold => &[ThresholdProcedure::SetProcedureThreshold],
            Self::UpdateGuardian => &[ThresholdProcedure::UpdateGuardian],
            Self::P2id => &[
                ThresholdProcedure::SendAsset,
                ThresholdProcedure::CreateNote,
            ],
            Self::ConsumeNotes => &[ThresholdProcedure::ReceiveAsset],
            Self::AccountDefault => &[],
        }
    }

    /// Whether the auth procedure needs GUARDIAN's acknowledgment: a guardian switch is
    /// authorized by the cosigners alone.
    pub fn requires_guardian_ack(self) -> bool {
        !matches!(self, Self::UpdateGuardian)
    }

    /// The threshold the chain holds this transaction to on `account`: the highest threshold
    /// among the procedures it calls, else the account default. `None` when the account
    /// carries no multisig threshold at all.
    pub fn threshold(self, account: &Account) -> Option<u32> {
        let default = default_threshold(account)?;
        Some(
            self.procedures()
                .iter()
                .map(|procedure| procedure_override(account, procedure.root()).unwrap_or(default))
                .max()
                .unwrap_or(default),
        )
    }
}

/// The account's default threshold, or `None` when it carries no multisig threshold slot.
pub fn default_threshold(account: &Account) -> Option<u32> {
    account
        .storage()
        .get_item(AuthGuardedMultisig::threshold_config_slot())
        .ok()
        .map(|config| config[0].as_canonical_u64() as u32)
}

/// The override stored for the procedure with MAST root `root`. A zero entry is no override.
pub fn procedure_override(account: &Account, root: Word) -> Option<u32> {
    account
        .storage()
        .get_map_item(
            AuthGuardedMultisig::procedure_thresholds_slot(),
            StorageMapKey::new(root),
        )
        .ok()
        .map(|entry| entry[0].as_canonical_u64() as u32)
        .filter(|threshold| *threshold != 0)
}
