/**
 * Wallet-facing orchestration of the note-recovery primitives.
 *
 * After key-based recovery the local Miden store starts empty (or, in a
 * shared store, its global cursors may already be past this account's
 * notes). Each recovery primitive rescans one source normal forward sync
 * would skip: the private-note transport backlog, the notes embedded in
 * pending consume-notes proposals, and the chain's historical public notes
 * for the account's tag. `Multisig.recoverNotes` runs them as a single flow
 * and finishes with a normal sync so recovered notes are verified and ready
 * to consume.
 *
 * The TS counterpart of the Rust SDK's `MultisigClient::recover_notes`,
 * with matching semantics and report shape.
 */

import { errorMessage } from '../connectivity.js';
import { RecoveryCancelledError } from './proposalNoteImport.js';
import type { NoteImportOutcome } from './proposalNoteImport.js';
import { BackfillRangeError, type PublicBackfillReport } from './publicNoteBackfill.js';
import type { TransportRecoveryReport } from './transportDrain.js';

/** One step of the note-recovery flow, used to attribute problems. */
export type RecoveryStep =
  | 'transport-drain'
  | 'proposal-import'
  | 'public-backfill'
  | 'sync';

/**
 * A step of the recovery flow that could not run (or, for the final sync,
 * did not finish). The flow always continues with the remaining steps; a
 * problem here means the corresponding report field is absent (or `synced`
 * is `false`) and rerunning the flow may recover more.
 */
export interface RecoveryStepProblem {
  /** The step that failed. */
  step: RecoveryStep;
  /** Human-readable cause. */
  reason: string;
  /**
   * Whether rerunning the flow can plausibly make this step succeed.
   * Failures reaching this level are I/O failures against GUARDIAN, the
   * node, or the local store; all but local-store failures are marked
   * retryable. The flow is idempotent, so retrying is always safe.
   */
  retryable: boolean;
}

/**
 * Result of `Multisig.recoverNotes`.
 *
 * Each strategy's field holds its primitive's own report and is present
 * exactly when the strategy was enabled and ran; an enabled strategy that
 * could not run at all is a {@link RecoveryStepProblem} instead. Step
 * problems never abort the flow.
 */
export interface NoteRecoveryReport {
  /** Report of the transport backlog drain, when that strategy ran. */
  transport?: TransportRecoveryReport;
  /**
   * Per-note outcomes of the proposal-embedded import, when that strategy
   * ran.
   */
  proposalImport?: NoteImportOutcome[];
  /** Report of the historical public-note backfill, when that strategy ran. */
  backfill?: PublicBackfillReport;
  /** Steps that could not run or finish. Empty on a fully clean flow. */
  problems: RecoveryStepProblem[];
  /** Whether the final verifying sync completed. */
  synced: boolean;
  /**
   * Total note records newly added to the local store by this flow: the
   * drain's imports plus every `imported` outcome from the proposal import
   * and the backfill.
   */
  imported: number;
  /**
   * Whether rerunning the flow can plausibly recover more: any step
   * problem, per-note outcome, or strategy report marked retryable.
   */
  retryable: boolean;
}

/**
 * Options for `Multisig.recoverNotes`. The default runs every strategy over
 * the full chain history and syncs afterwards.
 */
export interface RecoverNotesOptions {
  /**
   * Rescan the private-note transport backlog (the standalone
   * `drainPrivateNoteBacklog`). Default `true`.
   */
  transportDrain?: boolean;
  /**
   * Import the notes embedded in pending consume-notes proposals
   * (`Multisig.importNotesFromProposals`). Default `true`.
   */
  proposalImport?: boolean;
  /**
   * Scan chain history for public notes addressed at the account's tag
   * (`Multisig.backfillPublicNotesByTag`). Default `true`.
   */
  publicBackfill?: boolean;
  /** First block of the backfill scan; defaults to genesis. */
  fromBlock?: number;
  /** Last block of the backfill scan; defaults to the current chain tip. */
  toBlock?: number;
  /**
   * Run a normal sync after the strategies so imported notes are verified
   * and show up as consumable. Default `true`.
   */
  syncAfter?: boolean;
}

/**
 * The guardian-switch slice of {@link RecoverNotesOptions}: only the
 * proposal-embedded note import runs. Internal — the public entry point is
 * `Multisig.preservePreSwitchProposalNotes`, which adds the switch-specific
 * safety contract (timeout, cancellation, warnings) around this slice. The
 * `satisfies` clause forces every non-range option to be listed, so adding
 * a recovery strategy fails to compile until the switch path decides on it.
 */
export const GUARDIAN_SWITCH_RECOVERY_OPTIONS = {
  transportDrain: false,
  proposalImport: true,
  publicBackfill: false,
  syncAfter: false,
} satisfies Required<Omit<RecoverNotesOptions, 'fromBlock' | 'toBlock'>>;

/**
 * The strategy implementations `runNoteRecovery` orchestrates. `Multisig`
 * wires these to the SDK primitives; tests can substitute stubs.
 */
export interface NoteRecoverySteps {
  /** Drain the private-note transport backlog. */
  transportDrain: () => Promise<TransportRecoveryReport>;
  /** Import notes embedded in pending consume-notes proposals. */
  proposalImport: () => Promise<NoteImportOutcome[]>;
  /** Backfill historical public notes by tag. */
  publicBackfill: () => Promise<PublicBackfillReport>;
  /** Run the final verifying sync. */
  sync: () => Promise<void>;
}

function countImported(outcomes: readonly NoteImportOutcome[]): number {
  return outcomes.filter((o) => o.status === 'imported').length;
}

/**
 * Runs the enabled recovery strategies in order — transport drain, proposal
 * import, public backfill, final sync — folding each strategy-level throw
 * into a {@link RecoveryStepProblem} so no step failure aborts the flow.
 *
 * The optional `cancelled` token makes the run cooperatively cancellable:
 * the orchestrator checks it before each step, and a
 * {@link RecoveryCancelledError} from inside a step is recorded as one
 * non-retryable problem on the interrupted step, after which no further
 * step runs — cancellation never masquerades as a GUARDIAN or node outage.
 *
 * Throws only for an inverted backfill range (a caller error). See
 * `Multisig.recoverNotes` for the wallet-facing entry point.
 */
export async function runNoteRecovery(
  options: RecoverNotesOptions,
  steps: NoteRecoverySteps,
  cancelled?: () => boolean,
): Promise<NoteRecoveryReport> {
  if (
    options.fromBlock !== undefined &&
    options.toBlock !== undefined &&
    options.fromBlock > options.toBlock
  ) {
    throw new Error(
      `backfill range is inverted: fromBlock ${options.fromBlock} > toBlock ${options.toBlock}`,
    );
  }

  const report: NoteRecoveryReport = {
    problems: [],
    synced: false,
    imported: 0,
    retryable: false,
  };

  // Set once a cancellation is observed (token flipped, or a
  // RecoveryCancelledError surfaced from a step): the remaining steps are
  // skipped and exactly one problem records the interruption point.
  let stopped = false;
  const interrupted = (step: RecoveryStep, err: unknown): boolean => {
    if (!(err instanceof RecoveryCancelledError)) {
      return false;
    }
    report.problems.push({ step, reason: errorMessage(err), retryable: false });
    stopped = true;
    return true;
  };
  const startStep = (step: RecoveryStep): boolean => {
    if (stopped) {
      return false;
    }
    if (cancelled?.()) {
      report.problems.push({
        step,
        reason: 'note recovery stopped before completion by its caller',
        retryable: false,
      });
      stopped = true;
      return false;
    }
    return true;
  };

  if (options.transportDrain !== false && startStep('transport-drain')) {
    try {
      report.transport = await steps.transportDrain();
    } catch (err) {
      if (!interrupted('transport-drain', err)) {
        // The drain's contract: a throw means the local store itself failed,
        // which a rerun is unlikely to fix.
        report.problems.push({
          step: 'transport-drain',
          reason: errorMessage(err),
          retryable: false,
        });
      }
    }
  }

  if (options.proposalImport !== false && startStep('proposal-import')) {
    try {
      report.proposalImport = await steps.proposalImport();
    } catch (err) {
      if (!interrupted('proposal-import', err)) {
        report.problems.push({
          step: 'proposal-import',
          reason: `failed to list pending proposals: ${errorMessage(err)}`,
          retryable: true,
        });
      }
    }
  }

  if (options.publicBackfill !== false && startStep('public-backfill')) {
    try {
      report.backfill = await steps.publicBackfill();
    } catch (err) {
      if (!interrupted('public-backfill', err)) {
        // With the fully-explicit range validated above, a throw here is
        // normally the chain-tip lookup (a node RPC failure, worth retrying);
        // an invalid range against the resolved tip is a caller error and is
        // not — mirroring the Rust orchestrator's `InvalidConfig` check.
        report.problems.push({
          step: 'public-backfill',
          reason: errorMessage(err),
          retryable: !(err instanceof BackfillRangeError),
        });
      }
    }
  }

  if (options.syncAfter !== false && startStep('sync')) {
    try {
      await steps.sync();
      report.synced = true;
    } catch (err) {
      if (!interrupted('sync', err)) {
        report.problems.push({
          step: 'sync',
          reason: `recovery imports succeeded but the verifying sync failed; rerun a sync: ${errorMessage(err)}`,
          retryable: true,
        });
      }
    }
  }

  report.imported =
    (report.transport?.imported ?? 0) +
    countImported(report.proposalImport ?? []) +
    countImported(report.backfill?.outcomes ?? []);
  report.retryable =
    report.problems.some((p) => p.retryable) ||
    report.transport?.retryable === true ||
    (report.proposalImport ?? []).some((o) => o.retryable) ||
    report.backfill?.retryable === true;

  return report;
}
