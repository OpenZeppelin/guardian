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
import { isLikelyNetworkError } from './connectivity.js';

/**
 * Outcome class of a private-note transport backlog drain:
 * - `completed` — the full transport backlog was scanned; every note the
 *   transport still holds for the tracked tags is now in the local store.
 * - `unavailable` — the transport could not be consulted at all: it is
 *   disabled for the injected `MidenClient` (no `noteTransportUrl`
 *   configured) or unreachable. Nothing was scanned; the rest of a recovery
 *   flow should proceed without transport notes.
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
const TRANSPORT_DISABLED_FRAGMENT = 'note transport is disabled';
const PAGINATION_GUARD_FRAGMENT = 'did not converge';

function classifyDrainFailure(err: unknown): {
  status: TransportRecoveryStatus;
  retryable: boolean;
} {
  const message = (err as { message?: string } | null | undefined)?.message ?? String(err ?? '');
  const lower = message.toLowerCase();
  // No transport configured — retrying cannot help until the MidenClient is
  // rebuilt with a `noteTransportUrl`.
  if (lower.includes(TRANSPORT_DISABLED_FRAGMENT)) {
    return { status: 'unavailable', retryable: false };
  }
  // The upstream convergence guard tripped (the server cursor kept advancing
  // for 1000 iterations without an empty batch). The backlog is partially
  // imported; a rerun continues making progress because imports are
  // idempotent.
  if (lower.includes(PAGINATION_GUARD_FRAGMENT)) {
    return { status: 'failed', retryable: true };
  }
  // The transport exists but could not be reached — same class as disabled
  // for a recovery flow (nothing was scanned), but worth retrying once
  // connectivity returns.
  if (isLikelyNetworkError(err)) {
    return { status: 'unavailable', retryable: true };
  }
  return { status: 'failed', retryable: false };
}

/**
 * Rescans the full private-note transport backlog for every tracked note tag
 * and imports what it finds, regardless of the stored transport cursor
 * (issue #414). Mirrors `MultisigClient::drain_private_note_backlog` in the
 * Rust SDK.
 *
 * Use after account recovery: a fresh store has no transport cursor, and in
 * a shared store another account's sync may have advanced the cursor past
 * this account's notes. The drain is idempotent, tag-scoped (the recovered
 * account must already be in the store so its note tag is tracked —
 * `MultisigClient.load` does this), and never regresses an already-advanced
 * cursor.
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

  let failed = false;
  let failure: unknown;
  try {
    await midenClient.notes.fetchPrivate({ mode: 'all' });
  } catch (err) {
    failed = true;
    failure = err;
  }

  // Count even when the drain failed: each fetched batch is imported as it
  // arrives, so notes recovered before the failure stay in the store.
  // Records are never removed by a drain, so the length delta is the count
  // of newly imported records.
  const after = (await midenClient.notes.list()).length;
  const imported = Math.max(after - before, 0);

  if (!failed) {
    return { status: 'completed', imported, retryable: false };
  }

  const { status, retryable } = classifyDrainFailure(failure);
  const message =
    (failure as { message?: string } | null | undefined)?.message ?? String(failure ?? '');
  return { status, imported, retryable, reason: message };
}
