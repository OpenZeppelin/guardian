//! Which proposals a client must execute itself, even when GUARDIAN executes its proposals.

use std::collections::HashMap;
use std::fmt;

use miden_protocol::account::AccountId;
use miden_protocol::note::NoteType;

use crate::error::{MultisigError, Result};
use crate::proposal::{Proposal, TransactionType};

/// Why a proposal must be executed by a client rather than by GUARDIAN: local execution does
/// client-side work that GUARDIAN execution skips.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocalExecutionReason {
    /// The executing client finishes a GUARDIAN switch: it verifies the new endpoint, registers
    /// the account there and repoints itself.
    SwitchGuardian,
    /// Only the executing client holds the private output note it must hand to the recipient.
    PrivateNote,
}

impl LocalExecutionReason {
    /// Why a proposal of `transaction_type` must be executed locally, or `None` when GUARDIAN can
    /// execute it.
    pub fn of(transaction_type: &TransactionType) -> Option<Self> {
        match transaction_type {
            TransactionType::SwitchGuardian { .. } => Some(Self::SwitchGuardian),
            TransactionType::P2ID { note_type, .. } => match note_type {
                NoteType::Private => Some(Self::PrivateNote),
                NoteType::Public => None,
            },
            TransactionType::ConsumeNotes { .. }
            | TransactionType::AddCosigner { .. }
            | TransactionType::RemoveCosigner { .. }
            | TransactionType::UpdateProcedureThreshold { .. }
            | TransactionType::UpdateSigners { .. }
            | TransactionType::Custom => None,
        }
    }

    /// The stable identifier: `switch_guardian` or `private_note`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SwitchGuardian => "switch_guardian",
            Self::PrivateNote => "private_note",
        }
    }

    /// The reason a stable identifier names, or `None` for one this version does not know.
    pub fn parse(identifier: &str) -> Option<Self> {
        match identifier {
            "switch_guardian" => Some(Self::SwitchGuardian),
            "private_note" => Some(Self::PrivateNote),
            _ => None,
        }
    }

    /// A human-readable explanation.
    pub fn description(self) -> &'static str {
        match self {
            Self::SwitchGuardian => "a GUARDIAN switch is finished by the client that executes it",
            Self::PrivateNote => {
                "a private note can only be exported by the client that executed it"
            }
        }
    }

    fn waived_by(self, request: GuardianExecutionRequest) -> bool {
        match self {
            Self::SwitchGuardian => false,
            Self::PrivateNote => request.allow_private_note,
        }
    }
}

impl fmt::Display for LocalExecutionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Options for one [`request_guardian_execution`](crate::MultisigClient::request_guardian_execution)
/// call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GuardianExecutionRequest {
    /// Lets GUARDIAN execute a P2ID proposal that creates a private note. Set it only when this
    /// caller delivers the note to the recipient itself. A GUARDIAN switch is never waived.
    pub allow_private_note: bool,
}

impl GuardianExecutionRequest {
    /// Why a proposal of `transaction_type` must still be executed locally under this request, or
    /// `None` when GUARDIAN may execute it.
    pub fn local_execution_reason(
        self,
        transaction_type: &TransactionType,
    ) -> Option<LocalExecutionReason> {
        LocalExecutionReason::of(transaction_type).filter(|reason| !reason.waived_by(self))
    }
}

/// Why each proposal this client has listed, fetched, signed, created or imported must be
/// executed locally, keyed by account and proposal id. A proposal id fixes its transaction, so an
/// entry never goes stale: a proposal executed or deleted since is refused by GUARDIAN itself.
#[derive(Debug, Default)]
pub(crate) struct KnownProposals {
    reasons: HashMap<(AccountId, String), Option<LocalExecutionReason>>,
}

impl KnownProposals {
    fn key(account_id: AccountId, proposal_id: &str) -> (AccountId, String) {
        (
            account_id,
            proposal_id.trim_start_matches("0x").to_ascii_lowercase(),
        )
    }

    pub(crate) fn record(&mut self, account_id: AccountId, proposal: &Proposal) {
        self.insert(account_id, &proposal.id, &proposal.transaction_type);
    }

    pub(crate) fn insert(
        &mut self,
        account_id: AccountId,
        proposal_id: &str,
        transaction_type: &TransactionType,
    ) {
        self.reasons.insert(
            Self::key(account_id, proposal_id),
            LocalExecutionReason::of(transaction_type),
        );
    }

    /// Admits a GUARDIAN execution of a proposal this client holds. A proposal it does not hold
    /// is refused: the client cannot tell whether GUARDIAN may execute it.
    pub(crate) fn admit(
        &self,
        account_id: AccountId,
        proposal_id: &str,
        request: GuardianExecutionRequest,
    ) -> Result<()> {
        match self.reasons.get(&Self::key(account_id, proposal_id)) {
            Some(reason) => match reason.filter(|reason| !reason.waived_by(request)) {
                Some(reason) => Err(MultisigError::LocalExecutionRequired {
                    proposal_id: proposal_id.to_string(),
                    reason,
                }),
                None => Ok(()),
            },
            None => Err(MultisigError::ProposalNotHeldLocally {
                proposal_id: proposal_id.to_string(),
            }),
        }
    }
}
