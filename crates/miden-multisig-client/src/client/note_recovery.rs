//! Wallet-facing orchestration of the note-recovery primitives.
//!
//! After key-based recovery the local Miden store starts empty (or, in a
//! shared store, its global cursors may already be past this account's
//! notes). Each recovery primitive rescans one source normal forward sync
//! would skip: the private-note transport backlog, the notes embedded in
//! pending consume-notes proposals, and the chain's historical public notes
//! for the account's tag. [`MultisigClient::recover_notes`] runs them as a
//! single flow and finishes with a normal sync so recovered notes are
//! verified and ready to consume.

use super::MultisigClient;
use super::proposal_note_import::{NoteImportOutcome, NoteImportStatus};
use super::public_note_backfill::{PublicBackfillOptions, PublicBackfillReport};
use super::recovery::TransportRecoveryReport;
use crate::error::{MultisigError, Result, error_chain};

/// Options for [`MultisigClient::recover_notes`]. The default runs every
/// strategy over the full chain history and syncs afterwards; pass `None` to
/// the method for exactly that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteRecoveryOptions {
    /// Rescan the private-note transport backlog
    /// ([`MultisigClient::drain_private_note_backlog`]). Default `true`.
    pub transport_drain: bool,
    /// Import the notes embedded in pending consume-notes proposals
    /// ([`MultisigClient::import_notes_from_proposals`]). Default `true`.
    pub proposal_import: bool,
    /// Scan chain history for public notes addressed at the account's tag
    /// ([`MultisigClient::backfill_public_notes_by_tag`]). Default `true`.
    pub public_backfill: bool,
    /// Block range for the public backfill; the default scans genesis
    /// through the current chain tip.
    pub backfill: PublicBackfillOptions,
    /// Run a normal sync after the strategies so imported notes are verified
    /// and show up as consumable. Default `true`.
    pub sync_after: bool,
}

impl Default for NoteRecoveryOptions {
    fn default() -> Self {
        Self {
            transport_drain: true,
            proposal_import: true,
            public_backfill: true,
            backfill: PublicBackfillOptions::default(),
            sync_after: true,
        }
    }
}

/// One step of the note-recovery flow, used to attribute
/// [`RecoveryStepProblem`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryStep {
    /// The private-note transport backlog drain.
    TransportDrain,
    /// The proposal-embedded note import.
    ProposalImport,
    /// The historical public-note backfill.
    PublicBackfill,
    /// The final verifying sync.
    Sync,
}

impl RecoveryStep {
    /// Stable string form, shared with the TS SDK's `step` union.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TransportDrain => "transport-drain",
            Self::ProposalImport => "proposal-import",
            Self::PublicBackfill => "public-backfill",
            Self::Sync => "sync",
        }
    }
}

impl std::fmt::Display for RecoveryStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A step of the recovery flow that could not run (or, for the final sync,
/// did not finish). The flow always continues with the remaining steps; a
/// problem here means the corresponding report field is `None` (or `synced`
/// is `false`) and rerunning the flow may recover more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryStepProblem {
    /// The step that failed.
    pub step: RecoveryStep,
    /// Human-readable cause.
    pub reason: String,
    /// Whether rerunning the flow can plausibly make this step succeed.
    /// Failures reaching this level are I/O failures against GUARDIAN, the
    /// node, or the local store; all but store failures are marked
    /// retryable. The flow is idempotent, so retrying is always safe.
    pub retryable: bool,
}

/// Result of [`MultisigClient::recover_notes`].
///
/// Each strategy's field holds its primitive's own report and is `Some`
/// exactly when the strategy was enabled and ran; an enabled strategy that
/// could not run at all is a [`RecoveryStepProblem`] instead. Step problems
/// never abort the flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRecoveryReport {
    /// Report of the transport backlog drain, when that strategy ran.
    pub transport: Option<TransportRecoveryReport>,
    /// Per-note outcomes of the proposal-embedded import, when that strategy
    /// ran.
    pub proposal_import: Option<Vec<NoteImportOutcome>>,
    /// Report of the historical public-note backfill, when that strategy
    /// ran.
    pub backfill: Option<PublicBackfillReport>,
    /// Steps that could not run or finish. Empty on a fully clean flow.
    pub problems: Vec<RecoveryStepProblem>,
    /// Whether the final verifying sync completed.
    pub synced: bool,
    /// Total note records newly added to the local store by this flow: the
    /// drain's imports plus every `imported` outcome from the proposal
    /// import and the backfill.
    pub imported: usize,
    /// Whether rerunning the flow can plausibly recover more: any step
    /// problem, per-note outcome, or strategy report marked retryable.
    pub retryable: bool,
}

impl MultisigClient {
    /// Runs the note-recovery strategies as a single wallet-facing flow,
    /// typically right after key-based recovery loaded the account (via
    /// [`MultisigClient::pull_account`]).
    ///
    /// With `None` every strategy runs — the private-note transport backlog
    /// drain, the proposal-embedded note import, and the historical
    /// public-note backfill over the whole chain — followed by a normal sync
    /// that verifies whatever was imported. Pass [`NoteRecoveryOptions`] to
    /// choose strategies, bound the backfill's block range, or skip the
    /// final sync.
    ///
    /// No strategy failure aborts the flow: each primitive already reports
    /// per-note and per-source problems instead of throwing, and a strategy
    /// that cannot run at all (GUARDIAN unreachable while listing proposals,
    /// chain tip unresolvable, a broken local store) becomes a
    /// [`RecoveryStepProblem`] while the remaining strategies still run. The
    /// flow is idempotent — rerunning re-imports nothing that already
    /// arrived — so a report with `retryable: true` can simply be retried.
    ///
    /// # Errors
    ///
    /// Only for preconditions: no account is loaded, or the requested
    /// backfill range is inverted.
    ///
    /// # Example
    ///
    /// ```ignore
    /// client.pull_account(account_id).await?;
    /// let report = client.recover_notes(None).await?;
    /// println!("recovered {} notes", report.imported);
    /// ```
    pub async fn recover_notes(
        &mut self,
        options: Option<NoteRecoveryOptions>,
    ) -> Result<NoteRecoveryReport> {
        let options = options.unwrap_or_default();
        let account_id = self.require_account()?.id();
        if let (Some(from), Some(to)) = (options.backfill.from_block, options.backfill.to_block)
            && from > to
        {
            return Err(MultisigError::InvalidConfig(format!(
                "backfill range is inverted: from_block {from} > to_block {to}"
            )));
        }

        let mut report = NoteRecoveryReport {
            transport: None,
            proposal_import: None,
            backfill: None,
            problems: Vec::new(),
            synced: false,
            imported: 0,
            retryable: false,
        };

        if options.transport_drain {
            match self.drain_private_note_backlog().await {
                Ok(drain) => report.transport = Some(drain),
                // The drain's contract: an `Err` means the local store
                // itself failed, which a rerun is unlikely to fix.
                Err(e) => report.problems.push(RecoveryStepProblem {
                    step: RecoveryStep::TransportDrain,
                    reason: error_chain(&e),
                    retryable: false,
                }),
            }
        }

        if options.proposal_import {
            match self.list_proposals().await {
                Ok(proposals) => {
                    report.proposal_import =
                        Some(self.import_notes_from_proposals(&proposals).await);
                }
                Err(e) => report.problems.push(RecoveryStepProblem {
                    step: RecoveryStep::ProposalImport,
                    reason: format!("failed to list pending proposals: {}", error_chain(&e)),
                    retryable: true,
                }),
            }
        }

        if options.public_backfill {
            match self
                .backfill_public_notes_by_tag(account_id, Some(options.backfill))
                .await
            {
                Ok(backfill) => report.backfill = Some(backfill),
                // With the fully-explicit range validated above, an `Err`
                // here is normally the chain-tip lookup (a node RPC failure,
                // worth retrying); an invalid-range error against the
                // resolved tip is a caller error and is not.
                Err(e) => {
                    let retryable = !matches!(e, MultisigError::InvalidConfig(_));
                    report.problems.push(RecoveryStepProblem {
                        step: RecoveryStep::PublicBackfill,
                        reason: error_chain(&e),
                        retryable,
                    });
                }
            }
        }

        if options.sync_after {
            match self.sync().await {
                Ok(()) => report.synced = true,
                Err(e) => report.problems.push(RecoveryStepProblem {
                    step: RecoveryStep::Sync,
                    reason: format!("recovery imports succeeded but the verifying sync failed; rerun a sync: {}", error_chain(&e)),
                    retryable: true,
                }),
            }
        }

        let imported_outcomes = |outcomes: &[NoteImportOutcome]| {
            outcomes
                .iter()
                .filter(|o| o.status == NoteImportStatus::Imported)
                .count()
        };
        report.imported = report.transport.as_ref().map_or(0, |t| t.imported)
            + report.proposal_import.as_ref().map_or(0, |o| imported_outcomes(o))
            + report.backfill.as_ref().map_or(0, |b| imported_outcomes(&b.outcomes));
        report.retryable = report.problems.iter().any(|p| p.retryable)
            || report.transport.as_ref().is_some_and(|t| t.retryable)
            || report
                .proposal_import
                .as_ref()
                .is_some_and(|o| o.iter().any(|o| o.retryable))
            || report.backfill.as_ref().is_some_and(|b| b.retryable);

        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use miden_client::testing::mock::MockRpcApi;
    use miden_protocol::block::BlockNumber;
    use miden_protocol::note::NoteType;

    use super::super::recovery::TransportRecoveryStatus;
    use super::*;
    use crate::account::MultisigAccount;
    use crate::client::public_note_backfill::PublicBackfillOptions;
    use crate::client::test_support::{
        add_to_transport, mock_transport, offline_client, offline_client_with_node,
        p2id_note_for, test_wallet,
    };

    /// Loads `client` the way `pull_account` leaves it: the account is in
    /// the store (its note tag tracked) and cached as the current account.
    async fn load_account(client: &mut MultisigClient, seed: u8) {
        let account = test_wallet(seed);
        client.add_or_update_account(&account, true).await.unwrap();
        client.account = Some(MultisigAccount::new(account));
    }

    #[tokio::test]
    async fn recover_notes_requires_a_loaded_account() {
        let dir = tempfile::tempdir().unwrap();
        let mut client = offline_client(dir.path(), None).await;

        assert!(client.recover_notes(None).await.is_err());
    }

    #[tokio::test]
    async fn recover_notes_rejects_an_inverted_backfill_range() {
        let dir = tempfile::tempdir().unwrap();
        let mut client = offline_client(dir.path(), None).await;
        load_account(&mut client, 31).await;

        let err = client
            .recover_notes(Some(NoteRecoveryOptions {
                backfill: PublicBackfillOptions {
                    from_block: Some(BlockNumber::from(5u32)),
                    to_block: Some(BlockNumber::from(1u32)),
                },
                ..Default::default()
            }))
            .await
            .expect_err("inverted range must error");
        assert!(err.to_string().contains("inverted"));
    }

    /// The default flow keeps going when individual steps cannot run:
    /// GUARDIAN and the direct node channel are unreachable here, so the
    /// proposal listing, the backfill's tip lookup, and the final sync all
    /// fail — yet the transport drain still recovers its backlog, and every
    /// failed step lands in `problems` as retryable.
    #[tokio::test]
    async fn recover_notes_continues_past_failing_steps() {
        let dir = tempfile::tempdir().unwrap();
        let (node, api) = mock_transport();
        let mut client = offline_client(dir.path(), Some(api)).await;
        load_account(&mut client, 32).await;
        let account = client.account.as_ref().unwrap().inner().clone();
        add_to_transport(&node, p2id_note_for(&account, 1, NoteType::Private));

        let report = client.recover_notes(None).await.unwrap();

        let transport = report.transport.expect("drain ran");
        assert_eq!(transport.status, TransportRecoveryStatus::Completed);
        assert_eq!(transport.imported, 1);
        assert_eq!(report.proposal_import, None);
        assert_eq!(report.backfill, None);
        assert!(!report.synced);
        assert_eq!(report.imported, 1);
        assert!(report.retryable);

        let steps: Vec<RecoveryStep> = report.problems.iter().map(|p| p.step).collect();
        assert_eq!(
            steps,
            vec![
                RecoveryStep::ProposalImport,
                RecoveryStep::PublicBackfill,
                RecoveryStep::Sync,
            ]
        );
        assert!(report.problems.iter().all(|p| p.retryable));
    }

    #[tokio::test]
    async fn recover_notes_runs_only_the_selected_strategies() {
        let dir = tempfile::tempdir().unwrap();
        let (_node, api) = mock_transport();
        let mut client = offline_client(dir.path(), Some(api)).await;
        load_account(&mut client, 33).await;

        let report = client
            .recover_notes(Some(NoteRecoveryOptions {
                transport_drain: true,
                proposal_import: false,
                public_backfill: false,
                backfill: PublicBackfillOptions::default(),
                sync_after: false,
            }))
            .await
            .unwrap();

        let transport = report.transport.expect("drain ran");
        assert_eq!(transport.status, TransportRecoveryStatus::Completed);
        assert_eq!(report.proposal_import, None);
        assert_eq!(report.backfill, None);
        assert!(report.problems.is_empty());
        assert!(!report.synced);
        assert_eq!(report.imported, 0);
        assert!(!report.retryable);
    }

    /// With a reachable mock node the backfill strategy produces a real
    /// report, and a transport-less client reports the drain unavailable
    /// instead of failing the flow.
    #[tokio::test]
    async fn recover_notes_reports_backfill_and_unavailable_transport() {
        let dir = tempfile::tempdir().unwrap();
        let mut client =
            offline_client_with_node(dir.path(), Arc::new(MockRpcApi::default())).await;
        load_account(&mut client, 34).await;

        let report = client
            .recover_notes(Some(NoteRecoveryOptions {
                proposal_import: false,
                sync_after: false,
                ..Default::default()
            }))
            .await
            .unwrap();

        let transport = report.transport.expect("drain ran");
        assert_eq!(transport.status, TransportRecoveryStatus::Unavailable);
        let backfill = report.backfill.expect("backfill ran");
        assert_eq!(backfill.scanned_from, 0);
        assert!(backfill.uncovered.is_empty());
        assert!(report.problems.is_empty());
        assert_eq!(report.imported, 0);
        assert!(!report.retryable);
    }
}
