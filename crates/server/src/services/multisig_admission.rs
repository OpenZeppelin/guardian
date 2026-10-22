//! Multisig gate for `push_delta`. A candidate is acknowledged only when
//! the matching proposal carries verified cosigner signatures over the
//! pushed transaction, at least as many as the lowest threshold the
//! account permits for any transaction it can execute. The chain enforces the exact
//! threshold of the procedures a transaction calls; this floor never asks
//! for more than the chain will, so the gate never refuses a transaction
//! the chain would accept.

use std::collections::BTreeSet;

use miden_protocol::account::{Account, StorageMapKey, StorageSlotContent};
use miden_protocol::transaction::TransactionSummary;
use miden_standards::account::auth::AuthGuardedMultisig;
use serde_json::Value;

use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus};
use crate::error::{GuardianError, Result};
use crate::network::miden::account_inspector::MidenAccountInspector;
use crate::services::proposal_signature::{proposal_tx_summary, verify_proposal_signature};
use guardian_shared::FromJson;

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
    /// account's signature floor.
    pub(crate) fn authorize(
        &self,
        pushed_payload: &Value,
        proposal: Option<&DeltaObject>,
    ) -> Result<()> {
        let pushed =
            TransactionSummary::from_json(pushed_payload).map_err(GuardianError::InvalidDelta)?;
        let required = self.signature_floor()?;
        let Some(proposal) = proposal else {
            return self.reject(required, 0);
        };
        let summary = proposal_tx_summary(&proposal.delta_payload)?;
        if summary.to_commitment() != pushed.to_commitment() {
            return self.reject(required, 0);
        }
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

    /// The fewest signatures the chain accepts for any transaction on this
    /// account: the default threshold, or a lower override on one of the
    /// account's non-auth procedures. On chain (`multisig.masm`
    /// `compute_transaction_threshold`) every called non-auth procedure
    /// contributes its override or the default, combined by max, so no
    /// transaction needs fewer. The auth procedure at index 0 is skipped
    /// there, and a root the account code does not export is never called,
    /// so overrides on either never lower what a transaction needs and are
    /// ignored here. Which procedures a transaction calls is not visible in
    /// its summary, so the gate does not try to match the exact count: the
    /// chain enforces it. An override of zero is no override, on chain as
    /// here.
    fn signature_floor(&self) -> Result<usize> {
        let storage = self.account.storage();
        let default = storage
            .get_item(AuthGuardedMultisig::threshold_config_slot())
            .map_err(|_| {
                GuardianError::InvalidDelta("Multisig account is missing its threshold".to_string())
            })?[0]
            .as_canonical_u64() as usize;
        let lowest_override = match storage
            .get(AuthGuardedMultisig::procedure_thresholds_slot())
            .map(|slot| slot.content())
        {
            Some(StorageSlotContent::Map(overrides)) => self
                .account
                .code()
                .procedures()
                .iter()
                .skip(1)
                .map(|procedure| {
                    overrides.get(&StorageMapKey::new(*procedure.mast_root()))[0].as_canonical_u64()
                        as usize
                })
                .filter(|threshold| *threshold != 0)
                .min(),
            Some(StorageSlotContent::Value(_)) | None => None,
        };
        let floor = lowest_override.map_or(default, |lowest| lowest.min(default));
        if floor == 0 {
            return Err(GuardianError::InvalidDelta(
                "Multisig account threshold must be at least 1".to_string(),
            ));
        }
        Ok(floor)
    }

    fn reject(&self, required: usize, got: usize) -> Result<()> {
        tracing::info!(
            required,
            got,
            "Multisig delta is not authorized by enough verified cosigner signatures"
        );
        Err(GuardianError::InsufficientSignatures { required, got })
    }
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
