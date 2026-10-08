//! Multisig gate for `push_delta`. A candidate is acknowledged only when
//! the matching proposal carries enough verified cosigner signatures for
//! the account procedures the pushed transaction invokes.

use std::collections::BTreeSet;

use miden_protocol::Word;
use miden_protocol::account::{Account, StorageMapKey, StorageSlotName};
use miden_protocol::transaction::{RawOutputNote, TransactionSummary};
use miden_standards::account::auth::AuthGuardedMultisig;
use miden_standards::account::wallets::BasicWallet;
use miden_standards::note::StandardNote;
use serde_json::Value;

use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus};
use crate::error::{GuardianError, Result};
use crate::network::miden::account_inspector::MidenAccountInspector;
use crate::services::proposal_signature::{proposal_tx_summary, verify_proposal_signature};
use guardian_shared::FromJson;

/// An account procedure whose invocation sets the required signature
/// count. Procedures Guardian does not model are held to the account
/// default, matching the on-chain rule that a called procedure without
/// an override contributes the default threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum InvokedProcedure {
    UpdateSigners,
    SetProcedureThreshold,
    UpdateGuardian,
    SendAsset,
    ReceiveAsset,
    AccountDefault,
}

impl InvokedProcedure {
    fn of_proposal_type(proposal_type: Option<&str>) -> Self {
        match proposal_type {
            Some("add_signer" | "remove_signer" | "change_threshold") => Self::UpdateSigners,
            Some("update_procedure_threshold") => Self::SetProcedureThreshold,
            Some("switch_guardian") => Self::UpdateGuardian,
            Some("p2id") => Self::SendAsset,
            Some("consume_notes") => Self::ReceiveAsset,
            Some(_) | None => Self::AccountDefault,
        }
    }

    /// The procedures the summary shows were invoked, mirroring the
    /// on-chain derivation (`multisig.masm`: every called non-auth
    /// procedure contributes its threshold, combined by max). Input
    /// notes imply the wallet's receive procedure. Output notes imply
    /// its send procedure, except the fee note, which the auth
    /// procedure itself creates on every transaction. Patches to the
    /// auth component's configuration slots imply its update
    /// procedures, and a patch to any other slot is a procedure
    /// Guardian does not model, held to the account default. The
    /// executed-transactions slot is bookkeeping the auth procedure
    /// writes on every transaction and implies nothing.
    fn of_summary(summary: &TransactionSummary) -> BTreeSet<Self> {
        let mut invoked = BTreeSet::new();
        if summary.input_notes().num_notes() > 0 {
            invoked.insert(Self::ReceiveAsset);
        }
        if summary.output_notes().iter().any(is_non_fee_output_note) {
            invoked.insert(Self::SendAsset);
        }
        invoked.extend(
            summary
                .account_delta()
                .storage()
                .slots()
                .filter_map(|(slot, _)| Self::of_patched_slot(slot)),
        );
        invoked
    }

    fn of_patched_slot(slot: &StorageSlotName) -> Option<Self> {
        if slot == AuthGuardedMultisig::threshold_config_slot()
            || slot == AuthGuardedMultisig::approver_public_keys_slot()
            || slot == AuthGuardedMultisig::approver_scheme_ids_slot()
        {
            Some(Self::UpdateSigners)
        } else if slot == AuthGuardedMultisig::procedure_thresholds_slot() {
            Some(Self::SetProcedureThreshold)
        } else if slot == AuthGuardedMultisig::guardian_public_key_slot()
            || slot == AuthGuardedMultisig::guardian_scheme_id_slot()
        {
            Some(Self::UpdateGuardian)
        } else if slot == AuthGuardedMultisig::executed_transactions_slot() {
            None
        } else {
            Some(Self::AccountDefault)
        }
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

/// The fee note is public, so a partial (private) output note is never
/// the fee note.
fn is_non_fee_output_note(note: &RawOutputNote) -> bool {
    match note {
        RawOutputNote::Full(note) => !matches!(
            StandardNote::from_script(note.script()),
            Some(StandardNote::TX_FEE)
        ),
        RawOutputNote::Partial(_) => true,
    }
}

/// A parsed multisig account, present only when the tail state carries
/// the multisig threshold slot. Single-key states do not.
pub(crate) struct MultisigAccount {
    account: Account,
}

impl MultisigAccount {
    pub(crate) fn from_state(state_json: &Value) -> Option<Self> {
        // A state that does not parse is treated as single-key here:
        // `verify_delta` parses the same state right after this gate, so
        // such a push fails there and is never acknowledged.
        let Ok(account) = Account::from_json(state_json) else {
            tracing::warn!("Tail account state did not parse during multisig admission");
            return None;
        };
        if MidenAccountInspector::new(&account).has_multisig_auth() {
            Some(Self { account })
        } else {
            None
        }
    }

    /// Refuse the push unless `proposal` is the pending record for
    /// `pushed_payload` and its verified approver signatures meet the
    /// threshold of the procedures the pushed transaction invokes.
    pub(crate) fn authorize(
        &self,
        pushed_payload: &Value,
        proposal: Option<&DeltaObject>,
    ) -> Result<()> {
        let pushed =
            TransactionSummary::from_json(pushed_payload).map_err(GuardianError::InvalidDelta)?;
        let Some(proposal) = proposal else {
            return self.reject(self.required_threshold(&pushed, None)?, 0);
        };
        let summary = proposal_tx_summary(&proposal.delta_payload)?;
        if summary.to_commitment() != pushed.to_commitment() {
            return self.reject(self.required_threshold(&pushed, None)?, 0);
        }
        let claimed = invoked_procedure(&proposal.delta_payload);
        let required = self.required_threshold(&pushed, Some(claimed))?;
        let got = verified_approver_count(
            &summary,
            pending_signatures(proposal),
            &MidenAccountInspector::new(&self.account).extract_pubkeys(),
        );
        if got < required {
            return self.reject(required, got);
        }
        Ok(())
    }

    /// The signature count the pushed transaction needs: the maximum
    /// threshold over the procedures its summary shows were invoked and
    /// the procedure its proposal claims to invoke. A summary that shows
    /// no invoked procedure is held to the account default first, the
    /// on-chain fallback for a transaction that calls no non-auth
    /// procedure, so the claim is never trusted on its own: it can raise
    /// the requirement but never lower it, neither below what the
    /// summary shows nor below the default.
    fn required_threshold(
        &self,
        pushed: &TransactionSummary,
        claimed: Option<InvokedProcedure>,
    ) -> Result<usize> {
        let mut invoked = InvokedProcedure::of_summary(pushed);
        if invoked.is_empty() {
            invoked.insert(InvokedProcedure::AccountDefault);
        }
        invoked.extend(claimed);
        invoked
            .into_iter()
            .map(|procedure| self.threshold(procedure))
            .try_fold(0, |required, threshold| Ok(required.max(threshold?)))
    }

    fn threshold(&self, procedure: InvokedProcedure) -> Result<usize> {
        let threshold = effective_threshold(&self.account, procedure).ok_or_else(|| {
            GuardianError::InvalidDelta("Multisig account is missing its threshold".to_string())
        })?;
        if threshold == 0 {
            return Err(GuardianError::InvalidDelta(
                "Multisig account threshold must be at least 1".to_string(),
            ));
        }
        Ok(threshold)
    }

    fn reject(&self, required: usize, got: usize) -> Result<()> {
        tracing::info!(
            required,
            got,
            "Multisig delta is not authorized by a threshold of verified cosigner signatures"
        );
        Err(GuardianError::InsufficientSignatures { required, got })
    }
}

fn invoked_procedure(payload: &Value) -> InvokedProcedure {
    InvokedProcedure::of_proposal_type(
        payload
            .get("metadata")
            .and_then(|metadata| metadata.get("proposal_type"))
            .and_then(|proposal_type| proposal_type.as_str()),
    )
}

fn effective_threshold(account: &Account, procedure: InvokedProcedure) -> Option<usize> {
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

fn pending_signatures(proposal: &DeltaObject) -> &[CosignerSignature] {
    match &proposal.status {
        DeltaStatus::Pending { cosigner_sigs, .. } => cosigner_sigs,
        DeltaStatus::Candidate { .. }
        | DeltaStatus::Canonical { .. }
        | DeltaStatus::Retained { .. }
        | DeltaStatus::Discarded { .. } => &[],
    }
}

fn verified_approver_count(
    summary: &TransactionSummary,
    signatures: &[CosignerSignature],
    approvers: &[String],
) -> usize {
    let mut seen = BTreeSet::new();
    for signature in signatures {
        let Ok(signer) = verify_proposal_signature(summary, &signature.signature) else {
            continue;
        };
        if !approvers
            .iter()
            .any(|approver| approver.eq_ignore_ascii_case(&signer))
        {
            continue;
        }
        seen.insert(signer.to_ascii_lowercase());
    }
    seen.len()
}
