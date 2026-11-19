//! Shared types of the self-heal block. The runner loads [`AccountInputs`],
//! the planner turns them into an [`AccountPlan`], and the runner applies
//! the plan or records the account in the recovery file.

use std::path::PathBuf;

use serde::Serialize;

use crate::delta_object::DeltaObject;
use crate::metadata::AccountMetadata;
use crate::state_object::StateObject;
use crate::storage::{AccountHealWrite, ProposalRecord, SupersededDelta};

/// How the self-heal block runs at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelfHealMode {
    /// The block does not run.
    #[default]
    Off,
    /// The block plans every account, logs the plan and writes the
    /// recovery file. It writes nothing to storage.
    DryRun,
    /// The block plans every account and applies the plan.
    Apply,
}

/// Settings of the self-heal block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfHealConfig {
    pub mode: SelfHealMode,
    /// Where the recovery file is written.
    pub recovery_file: PathBuf,
    /// How many accounts are planned at the same time.
    pub max_concurrent_accounts: usize,
}

/// What the chain holds for an account whose stored commitment is not the
/// on-chain commitment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainRead {
    /// The chain holds this commitment, which is not the stored one.
    Mismatch { commitment: String },
    /// The chain has no state for the account.
    Absent,
}

/// Everything the planner needs for one account. All of it is read from
/// storage, except `chain`, which is the one RPC read.
#[derive(Debug, Clone)]
pub struct AccountInputs {
    pub metadata: AccountMetadata,
    /// The stored state, which is possibly forged.
    pub stored: StateObject,
    pub chain: ChainRead,
    /// Every row of the `deltas` table for the account, in any status.
    pub deltas: Vec<DeltaObject>,
    /// Every archived superseded delta of the account.
    pub superseded: Vec<SupersededDelta>,
    /// Every proposal of the account.
    pub proposals: Vec<ProposalRecord>,
    /// RFC 3339 timestamp for the statuses and rows the plan writes.
    pub now: String,
}

/// The decision for one account.
#[derive(Debug, Clone)]
pub enum AccountPlan {
    /// Nothing to repair and nothing to report.
    Skip { reason: &'static str },
    /// The true state was rebuilt and proven against the chain.
    Heal(Box<HealPlan>),
    /// The true state could not be proven. The account goes to the
    /// recovery file.
    Unrecovered(Box<UnrecoveredAccount>),
}

/// A proven repair.
#[derive(Debug, Clone)]
pub struct HealPlan {
    pub write: AccountHealWrite,
    /// The guardian public key commitment the true state binds. When it is
    /// not a key of this server, the runner releases the account after the
    /// write.
    pub guardian_commitment: Option<String>,
    /// Signer commitments of the true state, sorted.
    pub true_signers: Vec<String>,
    /// One entry for each row the plan changes or deliberately keeps, for
    /// the log.
    pub actions: Vec<RowAction>,
}

/// One row verdict, for the log.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RowAction {
    pub row: RowKind,
    /// The delta nonce or the proposal commitment.
    pub id: String,
    pub verdict: RowVerdict,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    Delta,
    Proposal,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RowVerdict {
    /// A delta on the proven path.
    Canonical,
    Keep,
    /// A proposal kept with fewer signatures.
    KeepWithFewerSignatures,
    Retain,
    Delete,
}

/// Why an account could not be repaired.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryReason {
    /// No row can build a base account: the account has no creation delta.
    NoBase,
    /// The chain has no state, a creation delta is not yet canonical, and
    /// the stored state is not the state that delta starts from.
    CreationDeltaPending,
    /// The rows end before the on-chain commitment.
    ReplayShort,
    /// The rows end before the on-chain commitment, and a
    /// `switch_guardian` row or proposal starts where they end. This is
    /// not proof that the account moved: an attacker can create such a
    /// row, so the account is paused and not released.
    LeftThisGuardian,
    /// Two rows that apply share one base and both lead on, or the rows do
    /// not form one chain to the on-chain commitment.
    AmbiguousHistory,
    /// A row that the replay needs does not decode.
    UndecodableRow,
    /// The chain read failed.
    RpcError,
}

/// One account entry of the recovery file. It carries commitments, nonces,
/// row links and signer commitments. It never carries payloads, signatures
/// or keys.
#[derive(Debug, Clone, Serialize)]
pub struct UnrecoveredAccount {
    pub account_id: String,
    pub reason: RecoveryReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub chain: ChainInfo,
    pub stored: StoredInfo,
    pub creation_delta: CreationDeltaInfo,
    pub replay: ReplayInfo,
    pub needed: NeededInfo,
    pub delta_rows: Vec<DeltaRowInfo>,
    pub proposals: Vec<ProposalInfo>,
    /// Whether the runner pauses the account. False only for
    /// [`RecoveryReason::RpcError`]. True for
    /// [`RecoveryReason::LeftThisGuardian`]: an attacker that was a stored
    /// cosigner can create a `switch_guardian` row or proposal, so such a
    /// row never lowers the protection of the account.
    #[serde(skip)]
    pub pause: bool,
    /// Whether the runner marks the account released. The planner never
    /// sets it: a stored row is not proof that the account left this
    /// guardian.
    #[serde(skip)]
    pub release: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ChainInfo {
    /// `mismatch`, `absent` or `unreadable`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commitment: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StoredInfo {
    pub commitment: String,
    pub nonce: Option<u64>,
    /// Signer commitments of the stored auth.
    pub signers: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CreationDeltaInfo {
    /// `none`, or the status of the creation delta row: `canonical`,
    /// `candidate`, `retained`, `discarded` or `superseded`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_commitment: Option<String>,
    /// Signer commitments of the account that the creation delta builds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReplayInfo {
    /// `creation_delta` or `none`.
    pub base: String,
    pub rows_applied: usize,
    /// Where the replay stopped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_commitment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_nonce: Option<u64>,
    /// The signer set at `last_commitment`. It was true at that state; it
    /// is not proven for the on-chain state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_signers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NeededInfo {
    /// The commitment of a full account state that lets the replay reach
    /// the chain: the base of the earliest row, or the on-chain commitment
    /// when no row links to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_with_commitment: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DeltaRowInfo {
    pub nonce: u64,
    /// `canonical`, `candidate`, `retained`, `discarded` or `superseded`.
    pub status: String,
    pub prev_commitment: String,
    pub new_commitment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_type: Option<String>,
    /// Whether the replay applied this row.
    pub applied: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProposalInfo {
    pub id: String,
    pub nonce: u64,
    pub base: String,
    pub proposer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_type: Option<String>,
    /// Signer commitment of each signature that verifies over the
    /// proposal. Whether the signer is a true signer is not known for an
    /// account that was not recovered.
    pub verified_signers: Vec<String>,
}

/// Totals of one run.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct SelfHealTotals {
    pub accounts: usize,
    pub healthy: usize,
    pub skipped: usize,
    pub healed: usize,
    pub not_recovered: usize,
}
