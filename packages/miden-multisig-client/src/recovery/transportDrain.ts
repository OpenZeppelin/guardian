/**
 * Recovery primitives for restoring note state after device loss.
 *
 * After recovery on a fresh device, or after adding a recovered account to a
 * shared store, the account's note tag has no note-transport cursor yet.
 * Since miden-client 0.17.1 the transport keeps one cursor per tag and scans
 * a tag without one from the first retained note, but each sync fetches a
 * bounded number of pages. The drain here keeps fetching until a fetch moves
 * no tag's cursor, so a recovery flow does not have to wait for later
 * background syncs.
 */

import type { MidenClient } from '@miden-sdk/miden-sdk';
import { errorMessage, isLikelyNetworkError } from '../connectivity.js';
import { isTransientRpcError } from '../rpc/errors.js';

/**
 * Outcome class of a private-note transport backlog drain:
 * - `completed` — the transport backlog was fetched until a fetch moved no
 *   tag's cursor; every note the transport still holds for the tracked tags
 *   is now in the local store.
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
   * drain. Can be non-zero even on `failed`: pages fetched before the
   * failure stay imported.
   */
  imported: number;
  /**
   * Whether rerunning the drain can plausibly succeed (transient
   * connectivity failures, a backlog longer than one drain's fetch budget).
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
 * `NoteTransportError::Network`'s Display prefix — the transport service
 * answered with an error. Exported for the drift-guard test.
 */
export const TRANSPORT_NETWORK_FRAGMENT = 'note transport network error';
/**
 * `NoteTransportError::Connection`'s Display prefix — endpoint parsing, TLS
 * configuration, and actual connect failures, indiscriminately. Exported for
 * the drift-guard test.
 */
export const TRANSPORT_CONNECTION_FRAGMENT = 'connection error';
/**
 * `RpcError::RequestError`'s Display prefix — a NODE RPC failure (each
 * fetched batch is imported through the node, so this is a mid-drain import
 * failure, not a transport outage). Exported for the drift-guard test.
 */
export const NODE_RPC_FRAGMENT = 'grpc request failed';
/**
 * `RpcError::ConnectionError`'s Display text — the NODE could not be
 * reached at all, which mid-drain is the same interrupted-import class as
 * {@link NODE_RPC_FRAGMENT}. Exported for the drift-guard test.
 */
export const NODE_CONNECT_FRAGMENT = 'failed to connect to the Miden node';
/**
 * Mirror of the Rust SDK's `MAX_DRAIN_PASSES`: upper bound on transport
 * fetches one drain runs. Each fetch reads up to 32 pages per request group
 * of up to 128 tags; a backlog whose cursors still move after this many
 * fetches is reported as a retryable failure, and the stored per-tag cursors
 * let the next drain or sync continue from there.
 */
const MAX_DRAIN_PASSES = 64;

/**
 * miden-client's `NOTE_TRANSPORT_CURSORS_KEY`, a setting in its client-owned
 * scope (`SettingScope::Client` = 0), which the SDK's `settings` API does not
 * reach. Exported for the drift-guard test, which pins the key and the
 * store layout below against the shipped SDK.
 */
export const NOTE_TRANSPORT_CURSORS_KEY = 'note_transport_cursors';
const CLIENT_SETTING_SCOPE = 0;
/** The SDK's IndexedDB table for settings, keyed by `[scope, key]`. */
const SETTINGS_TABLE = 'settings';

/**
 * The stored per-tag note-transport cursors, as the raw bytes miden-client
 * keeps, read from the client's IndexedDB store. Compared, never decoded: a
 * fetch that leaves them unchanged found every tracked tag's history
 * exhausted. Throws when the store cannot be read, which is a local-store
 * failure the caller must see.
 */
export async function readTransportCursors(midenClient: MidenClient): Promise<Uint8Array | null> {
  const name = await midenClient.storeIdentifier();
  const db: IDBDatabase = await new Promise((resolve, reject) => {
    const request = indexedDB.open(name);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  try {
    if (!db.objectStoreNames.contains(SETTINGS_TABLE)) {
      throw new Error(`storage error: the store ${name} has no ${SETTINGS_TABLE} table`);
    }
    const row: { value?: Uint8Array } | undefined = await new Promise((resolve, reject) => {
      const request = db
        .transaction(SETTINGS_TABLE, 'readonly')
        .objectStore(SETTINGS_TABLE)
        .get([CLIENT_SETTING_SCOPE, NOTE_TRANSPORT_CURSORS_KEY]);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    return row?.value ?? null;
  } finally {
    db.close();
  }
}

function sameBytes(a: Uint8Array | null, b: Uint8Array | null): boolean {
  if (a === null || b === null) return a === b;
  return a.length === b.length && a.every((byte, index) => byte === b[index]);
}

/**
 * Mirror of the Rust SDK's `CONNECT_PERMANENT_SIGNALS`: connection-failure
 * wording a retry cannot fix (misconfigured endpoint, TLS/certificate
 * problems). Everything else connection-shaped is the peer-still-booting
 * case and stays retryable.
 */
const CONNECT_PERMANENT_SIGNALS = ['certificate', 'tls', 'invalid uri', 'unsupported scheme'];
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
  // The upstream convergence guard of miden-client before 0.17.1, which no
  // longer raises it. A rerun is safe (imports are idempotent).
  if (lower.includes(PAGINATION_GUARD_FRAGMENT)) {
    return { status: 'failed', retryable: true, reason, scanned: true };
  }
  // A NODE RPC failure: each fetched batch is imported through the node
  // (inclusion-proof lookup), so this interrupted the drain mid-way — the
  // transport itself was reachable. Mirrors the Rust `ClientError::RpcError`
  // arm: report `failed`, with the shared RPC classifier deciding whether a
  // rerun can help.
  if (lower.includes(NODE_RPC_FRAGMENT) || lower.includes(NODE_CONNECT_FRAGMENT.toLowerCase())) {
    return { status: 'failed', retryable: isTransientRpcError(err), reason, scanned: true };
  }
  // The transport answered with an error — worth retrying once the service
  // recovers.
  if (lower.includes(TRANSPORT_NETWORK_FRAGMENT)) {
    return { status: 'unavailable', retryable: true, reason, scanned: true };
  }
  // `Connection` wraps endpoint parsing, TLS configuration, and actual
  // connect failures indiscriminately; permanent wording (mirroring the Rust
  // SDK's cause-chain classifier) must not tell a recovery flow to loop
  // retrying a client that can never connect.
  if (lower.includes(TRANSPORT_CONNECTION_FRAGMENT)) {
    const permanent = CONNECT_PERMANENT_SIGNALS.some((signal) => lower.includes(signal));
    return { status: 'unavailable', retryable: !permanent, reason, scanned: true };
  }
  // Remaining connectivity-shaped wording (e.g. a raw fetch failure): the
  // transport could not be reached; worth retrying once connectivity
  // returns.
  if (isLikelyNetworkError(err)) {
    return { status: 'unavailable', retryable: true, reason, scanned: true };
  }
  return { status: 'failed', retryable: false, reason, scanned: true };
}

/**
 * Fetches the private-note transport backlog for every tracked note tag and
 * imports what it finds. Counterpart of
 * `MultisigClient::drain_private_note_backlog` in the Rust SDK; the WASM
 * boundary exposes only error message text, so failure classification here
 * is message-based, keyed on the upstream Display prefixes pinned by the
 * drift-guard test.
 *
 * Use after account recovery, passing the **same** `MidenClient` instance
 * that was injected into `MultisigClient`. The recovered account must already
 * be in the store so its note tag is tracked (`MultisigClient.load` does
 * this). Its tag has no transport cursor yet, so the transport serves it from
 * the first retained note. The drain runs transport fetches until one moves
 * no tag's cursor, which only an exhausted backlog leaves unchanged: a fetch
 * whose pages were all duplicates or screened out still advances it. The
 * drain is idempotent: imports dedupe, and per-tag cursors only move forward.
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
  const noteCount = async (): Promise<number> => (await midenClient.notes.list()).length;
  const before = await noteCount();
  // Records are never removed by a drain, so the length delta is the count
  // of newly imported records. Caveat: it is a store delta — notes imported
  // by concurrent activity on the same store (a background sync, another
  // tab) during the drain are attributed to it.
  const importedSince = async (): Promise<number> => Math.max((await noteCount()) - before, 0);

  let last = await readTransportCursors(midenClient);
  for (let pass = 0; pass < MAX_DRAIN_PASSES; pass += 1) {
    try {
      // fetchPrivate throws the upstream disabled error when no transport is
      // configured, where syncNoteTransport silently no-ops; this drain must
      // report `unavailable`. A failed fetch still imports the pages fetched
      // before it.
      await midenClient.notes.fetchPrivate();
    } catch (err) {
      // A broken local store is an environment failure, not a transport
      // outcome: the whole recovery flow needs to know, so it propagates
      // (matching the Rust `StoreError` branch) instead of being folded into
      // the report.
      if (isLocalStoreError(err)) {
        throw err;
      }
      const { status, retryable, reason, scanned } = classifyDrainFailure(err);
      // A disabled transport throws before fetching anything, so skip the
      // re-count entirely.
      const imported = scanned ? await importedSince() : 0;
      if (status === 'unavailable' && imported > 0) {
        // `unavailable` promises "nothing was imported"; pages imported before
        // the failure make it an interrupted drain, so report it as a failure,
        // keeping the classified retryability, like the Rust twin.
        return { status: 'failed', imported, retryable, reason };
      }
      return { status, imported, retryable, reason };
    }
    const cursors = await readTransportCursors(midenClient);
    if (sameBytes(cursors, last)) {
      return { status: 'completed', imported: await importedSince(), retryable: false };
    }
    last = cursors;
  }

  return {
    status: 'failed',
    imported: await importedSince(),
    retryable: true,
    reason: `the transport backlog was not exhausted after ${MAX_DRAIN_PASSES} fetches; rerun the drain to continue from the stored cursors`,
  };
}
