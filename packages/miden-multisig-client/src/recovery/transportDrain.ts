/**
 * Recovery primitives for restoring note state after device loss.
 *
 * After recovery on a fresh device the local store has no note-transport
 * cursor — and in a shared dirty store another account's sync may have
 * advanced the cursor past notes belonging to the newly recovered account.
 * The primitives here rescan sources that normal forward sync would skip.
 */

import type { MidenClient } from '@miden-sdk/miden-sdk';
import { errorMessage, isLikelyNetworkError } from '../connectivity.js';

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
/**
 * `ClientError::StoreError`'s Display prefix in the WASM error chain.
 * Exported for the drift-guard test, which pins it against the shipped WASM
 * binary.
 */
export const STORE_ERROR_FRAGMENT = 'storage error';

/**
 * IndexedDB/Dexie error names that mean the local store itself failed —
 * these must reject (matching the Rust `StoreError` branch), never be folded
 * into a transport report.
 */
const STORE_ERROR_NAMES = ['QuotaExceededError', 'DatabaseClosedError', 'ReadOnlyError', 'TransactionInactiveError'];

/**
 * Does this failure mean the local store is broken (as opposed to a
 * transport/node problem)? Kept deliberately narrow: `AbortError` counts
 * only when its message points at an IndexedDB transaction/database abort —
 * network requests also abort, and those stay transport-classified.
 */
function isLocalStoreError(err: unknown): boolean {
  const message = errorMessage(err);
  const lower = message.toLowerCase();
  if (lower.includes(STORE_ERROR_FRAGMENT)) return true;
  const name =
    typeof err === 'object' && err !== null && 'name' in err && typeof err.name === 'string'
      ? err.name
      : undefined;
  if (name !== undefined && STORE_ERROR_NAMES.includes(name)) return true;
  // The WASM bridge may stringify the underlying store error into the
  // message rather than preserving the name.
  if (STORE_ERROR_NAMES.some((storeName) => message.includes(storeName))) return true;
  if (name === 'AbortError' && (lower.includes('transaction') || lower.includes('database'))) {
    return true;
  }
  return false;
}

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
 * and imports what it finds, regardless of the stored transport
 * cursor. Counterpart of `MultisigClient::drain_private_note_backlog`
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
    // The incremental fetch doubles as the transport probe: miden-sdk 0.16's
    // syncNoteTransport silently no-ops when the transport is disabled, but
    // this drain must report `unavailable`, and fetchPrivate still throws
    // the upstream disabled error.
    await midenClient.notes.fetchPrivate();
    // miden-sdk 0.16 replaced the explicit full drain with covered-tag
    // bookkeeping inside syncNoteTransport: every tag not yet marked covered
    // is drained from the start with a local cursor (the global cursor is
    // never regressed), then the steady-state fetch runs. Clearing the
    // covered-tags marker first forces that full per-tag re-drain — exactly
    // the recovery semantic this primitive promises. Imports dedupe, so
    // re-draining already-seen history is harmless. The key mirrors
    // miden-client's `NOTE_TRANSPORT_COVERED_TAGS_KEY`, which the JS surface
    // does not re-export.
    await midenClient.settings.remove('note_transport_covered_tags');
    await midenClient.syncNoteTransport();
  } catch (err) {
    // A broken local store is an environment failure, not a transport
    // outcome: the whole recovery flow needs to know, so it propagates
    // (matching the Rust `StoreError` branch) instead of being folded into
    // the report.
    if (isLocalStoreError(err)) {
      throw err;
    }
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
