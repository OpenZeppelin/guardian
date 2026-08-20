/**
 * Recovery primitives for restoring note state after device loss.
 *
 * After recovery on a fresh device the local store has no note-transport
 * cursor — and in a shared dirty store another account's sync may have
 * advanced the cursor past notes belonging to the newly recovered account.
 * The primitives here rescan sources that normal forward sync would skip
 * (issue #414, sub-issue of #357).
 */

import type { MidenClient } from '@miden-sdk/miden-sdk';
import { errorMessage, isLikelyNetworkError } from './connectivity.js';

/**
 * Outcome class of a private-note transport backlog drain:
 * - `completed` — the full transport backlog was scanned; every note the
 *   transport still holds for the tracked tags is now in the local store.
 * - `unavailable` — the transport could not be consulted at all: it is
 *   disabled for the injected `MidenClient` (no `noteTransportUrl`
 *   configured) or unreachable before anything was imported (`imported` is
 *   always 0 — a connection lost mid-drain after partial progress reports
 *   `failed` instead). The rest of a recovery flow should proceed without
 *   transport notes.
 * - `failed` — the drain started but did not finish; the backlog may be
 *   partially imported. `retryable` distinguishes transient failures (rerun
 *   the drain) from permanent ones.
 */
export type TransportRecoveryStatus = 'completed' | 'unavailable' | 'failed';

/**
 * Result of {@link drainPrivateNoteBacklog}.
 *
 * Transport problems are reported here rather than thrown so a transport
 * failure never aborts the rest of a recovery flow.
 */
export interface TransportRecoveryReport {
  /** Outcome class of the drain. */
  status: TransportRecoveryStatus;
  /**
   * Number of note records newly imported into the local store by this
   * drain. Can be non-zero even on `failed`: batches imported before the
   * failure stay imported.
   */
  imported: number;
  /**
   * Whether rerunning the drain can plausibly succeed (transient
   * connectivity failures, the upstream pagination convergence guard).
   * Always `false` on `completed`.
   */
  retryable: boolean;
  /** Human-readable cause when the drain did not complete. */
  reason?: string;
}

/**
 * The WASM client surfaces the upstream transport errors as plain messages,
 * so the message text is the only stable join key (same approach as
 * `connectivity.ts`). These fragments come from miden-client's
 * `NoteTransportError` `Display` impls.
 */
/** Exported for the drift-guard test, which pins them against the shipped WASM binary. */
export const TRANSPORT_DISABLED_FRAGMENT = 'note transport is disabled';
/** Exported for the drift-guard test, which pins them against the shipped WASM binary. */
export const PAGINATION_GUARD_FRAGMENT = 'did not converge';

interface DrainFailure {
  status: TransportRecoveryStatus;
  retryable: boolean;
  reason: string;
  /**
   * `false` when the failure proves nothing was fetched (disabled transport
   * throws before any scan), so the store does not need re-counting.
   */
  scanned: boolean;
}

function classifyDrainFailure(err: unknown): DrainFailure {
  const reason = errorMessage(err);
  const lower = reason.toLowerCase();
  // No transport configured — retrying cannot help until the MidenClient is
  // rebuilt with a `noteTransportUrl`. Thrown before anything is fetched.
  if (lower.includes(TRANSPORT_DISABLED_FRAGMENT)) {
    return { status: 'unavailable', retryable: false, reason, scanned: false };
  }
  // The upstream convergence guard tripped (the server cursor kept advancing
  // for 1000 iterations without an empty batch) — a server-side bug, not an
  // honest backlog. Retryable in the sense that a rerun is safe (imports are
  // idempotent) and succeeds once the server recovers.
  if (lower.includes(PAGINATION_GUARD_FRAGMENT)) {
    return { status: 'failed', retryable: true, reason, scanned: true };
  }
  // Connectivity-shaped wording: the transport (or the node, mid-import —
  // the message text cannot tell them apart) could not be reached; worth
  // retrying once connectivity returns.
  if (isLikelyNetworkError(err)) {
    return { status: 'unavailable', retryable: true, reason, scanned: true };
  }
  return { status: 'failed', retryable: false, reason, scanned: true };
}

/**
 * Rescans the full private-note transport backlog for every tracked note tag
 * and imports what it finds, regardless of the stored transport cursor
 * (issue #414). Counterpart of `MultisigClient::drain_private_note_backlog`
 * in the Rust SDK; note that the WASM boundary exposes only error message
 * text, so failure classification here is message-based and cannot always
 * distinguish a node connectivity failure mid-import from a transport
 * connectivity failure — both classes are reported retryable.
 *
 * Use after account recovery, passing the **same** `MidenClient` instance
 * that was injected into `MultisigClient`: a fresh store has no transport
 * cursor, and in a shared store another account's sync may have advanced the
 * cursor past this account's notes. The drain is idempotent, tag-scoped (the
 * recovered account must already be in the store so its note tag is tracked
 * — `MultisigClient.load` does this), and never regresses an
 * already-advanced cursor.
 *
 * Transport recovery is bounded by the transport service's retention:
 * senders may bypass the transport entirely and relayed blobs are pruned
 * after the retention window, so this is a best-effort rescan, **not** a
 * backup. Transport-disabled and transport-unreachable outcomes are reported
 * in the {@link TransportRecoveryReport} rather than thrown; this function
 * only throws when the local store itself fails.
 */
export async function drainPrivateNoteBacklog(
  midenClient: MidenClient,
): Promise<TransportRecoveryReport> {
  const before = (await midenClient.notes.list()).length;
  // Records are never removed by a drain, so the length delta is the count
  // of newly imported records. Caveat: it is a store delta — notes imported
  // by concurrent activity on the same store (a background sync, another
  // tab) during the drain are attributed to it.
  const importedSince = async (): Promise<number> =>
    Math.max((await midenClient.notes.list()).length - before, 0);

  try {
    await midenClient.notes.fetchPrivate({ mode: 'all' });
  } catch (err) {
    const { status, retryable, reason, scanned } = classifyDrainFailure(err);
    // Count even when the drain failed: each fetched batch is imported as it
    // arrives, so notes recovered before the failure stay in the store. A
    // disabled transport throws before fetching anything, so skip the
    // re-count entirely.
    const imported = scanned ? await importedSince() : 0;
    if (status === 'unavailable' && imported > 0) {
      // `unavailable` promises "nothing was imported"; a connection lost
      // mid-drain after partial progress is an interrupted drain, so report
      // it as a retryable failure instead.
      return { status: 'failed', imported, retryable: true, reason };
    }
    return { status, imported, retryable, reason };
  }

  return { status: 'completed', imported: await importedSince(), retryable: false };
}
