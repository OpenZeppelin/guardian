/**
 * Historical public-note backfill by tag (issue #416, sub-issue of #357).
 *
 * Public notes addressed to an account are on chain, but normal forward sync
 * starts from the store's **global** cursor: in a shared dirty store the
 * cursor may already be past blocks containing a recovered account's notes,
 * and a fresh store has no efficient path to them at all. This module
 * rescans a historical block range with the account's standard note tag and
 * imports what it finds with on-chain inclusion proofs, without ever
 * touching the global sync height.
 */

import {
  AccountId,
  type CommittedNote,
  Endpoint,
  type InputNoteRecord,
  type Note,
  type NoteInclusionProof,
  NoteTag,
  NoteType,
  RpcClient,
} from '@miden-sdk/miden-sdk';

import { errorMessage } from './connectivity.js';
import {
  collectExistingRecords,
  detailsKeyOf,
  errorDetail,
  importNoteWithProof,
  type NoteImportOutcome,
  reclassifyConsumedImports,
} from './proposalNoteImport.js';
import { getRawMidenClient, requireMidenRpcEndpoint, type RawClientSource } from './raw-client.js';
import { resolveRpcConfig, type RpcConfig } from './rpc/config.js';
import { isTransientRpcError } from './rpc/errors.js';
import { retryRpcRead } from './rpc/retry.js';
import { normalizeHexWord } from './utils/encoding.js';

/**
 * `RpcError::PaginationError`'s Display prefix in the WASM error chain: the
 * node caps internal pagination per `syncNotes` request, and the scan splits
 * the range client-side when it trips. Exported for the drift-guard test,
 * which pins it against the shipped WASM binary.
 */
export const RPC_PAGINATION_FRAGMENT = 'rpc pagination error';

/**
 * Upper bound on `syncNotes` requests per backfill. Splitting around the
 * node's pagination cap halves ranges, so the budget is only approachable
 * when nearly every sub-range is dense enough to trip the cap; exhausting it
 * reports the remaining ranges as uncovered instead of scanning forever.
 */
const MAX_SCAN_REQUESTS = 128;

/** Block numbers are u32 on chain; out-of-range JS numbers would silently
 * wrap modulo 2^32 at the WASM boundary and scan the wrong range. */
const MAX_BLOCK_NUMBER = 4_294_967_295;

function requireBlockNumber(name: string, value: number): void {
  if (!Number.isInteger(value) || value < 0 || value > MAX_BLOCK_NUMBER) {
    throw new Error(`${name} must be an integer in [0, ${MAX_BLOCK_NUMBER}], got ${value}`);
  }
}

/** A contiguous block range, inclusive on both ends. */
export interface BlockRange {
  /** First block of the range. */
  from: number;
  /** Last block of the range. */
  to: number;
}

/**
 * Result of {@link backfillPublicNotesByTag}.
 *
 * Scan problems are reported here rather than thrown so a partially failing
 * scan never aborts the rest of a recovery flow: notes discovered in the
 * covered ranges are imported regardless.
 */
export interface PublicBackfillReport {
  /** First block of the requested scan range. */
  scannedFrom: number;
  /** Last block of the requested scan range. */
  scannedTo: number;
  /** Unique tag-matching notes the scan discovered, of every visibility. */
  discovered: number;
  /** Unique non-public matches skipped: the chain does not hold their
   * bodies, so they cannot be rebuilt from a scan. Private notes are covered
   * by the transport drain and proposal-import primitives instead. */
  skippedPrivate: number;
  /** One outcome per unique public note discovered. */
  outcomes: NoteImportOutcome[];
  /** Sub-ranges of `[scannedFrom, scannedTo]` the scan could not cover (RPC
   * failures, or the scan budget ran out while splitting around the node's
   * pagination cap). Empty when the whole range was scanned. Notes committed
   * in these ranges may be missing from `outcomes`. */
  uncovered: BlockRange[];
  /** Whether rerunning the backfill can plausibly improve the result: cover
   * `uncovered` ranges, or retry outcomes whose own `retryable` flag is set.
   * Always `false` when the scan fully covered the range and no outcome is
   * retryable. */
  retryable: boolean;
  /** Human-readable cause when the scan did not cover the whole range. */
  reason?: string;
}

export interface BackfillPublicNotesOptions {
  /** Hex ID of the account whose standard note tag should be scanned. */
  accountId: string;
  /** Miden node RPC endpoint used for the scan and body fetches. Must point
   * at the same network as the injected Miden client. */
  midenRpcEndpoint: string;
  /** First block of the scan range (default: genesis). */
  fromBlock?: number;
  /** Last block of the scan range (default: the current chain tip). */
  toBlock?: number;
  /** Node RPC read-retry configuration (defaults match the rest of the SDK). */
  rpc?: RpcConfig;
}

/**
 * Scans a historical block range for public notes addressed at an account's
 * standard note tag and imports what it finds with their on-chain inclusion
 * proofs (issue #416). Counterpart of
 * `MultisigClient::backfill_public_notes_by_tag` in the Rust SDK.
 *
 * Use after account recovery: normal forward sync starts from the store's
 * **global** cursor, so in a shared dirty store the cursor may already be
 * past blocks containing the recovered account's notes, and a fresh store
 * would need to replay the whole chain state to see them. The scan is
 * tag-scoped and its cost grows with the number of matching notes, not the
 * range length (spike #412), which makes genesis an acceptable default lower
 * bound. The global sync height is never touched — run normal sync
 * afterwards to verify the imported notes. The store must have synced at
 * least once (`MultisigClient.load` does this): importing a proof into a
 * store that has never seen the chain fails, and such failures surface as
 * `failed` outcomes.
 *
 * Notes are discovered by tag only — a best-effort filter: notes sent with
 * unrelated custom tags are outside this scan's guarantee, and unrelated
 * notes whose tag collides with the account's are imported harmlessly (they
 * simply never become consumable). Only public notes can be rebuilt from
 * chain data; private matches are counted as `skippedPrivate` and are
 * covered by the transport drain and proposal-import primitives instead.
 *
 * A range dense enough to trip the node's internal pagination cap is split
 * client-side and rescanned as narrower requests; ranges that still cannot
 * be covered are reported in {@link PublicBackfillReport.uncovered} rather
 * than failing the recovery flow. This function throws only when the scan
 * range itself cannot be established (chain-tip lookup failed, an invalid
 * account ID, a block bound that is not a u32 integer, or
 * `fromBlock > toBlock`).
 *
 * @example
 * ```typescript
 * const report = await backfillPublicNotesByTag(midenClient, {
 *   accountId: multisig.accountId,
 *   midenRpcEndpoint: 'https://rpc.devnet.miden.io',
 * });
 * console.log(report.discovered, 'discovered,', report.outcomes.length, 'public');
 * await multisig.syncState(); // verifies the imported notes
 * ```
 */
export async function backfillPublicNotesByTag(
  midenClient: RawClientSource,
  options: BackfillPublicNotesOptions,
): Promise<PublicBackfillReport> {
  const midenRpcEndpoint = requireMidenRpcEndpoint(options.midenRpcEndpoint);
  const rpcConfig = resolveRpcConfig(options.rpc);
  const webClient = await getRawMidenClient(midenClient, midenRpcEndpoint);
  const rpcClient = new RpcClient(new Endpoint(midenRpcEndpoint));
  // Parse eagerly so a malformed account ID throws before any network work.
  AccountId.fromHex(options.accountId);

  const from = options.fromBlock ?? 0;
  requireBlockNumber('fromBlock', from);
  let to: number;
  if (options.toBlock !== undefined) {
    requireBlockNumber('toBlock', options.toBlock);
    to = options.toBlock;
  } else {
    try {
      const tip = await retryRpcRead(() => rpcClient.getBlockHeaderByNumber(), rpcConfig);
      to = tip.blockNum();
    } catch (error) {
      throw new Error(
        `failed to resolve the chain tip for the backfill scan: ${errorDetail(error)}`,
      );
    }
  }
  if (from > to) {
    throw new Error(`backfill range is inverted: fromBlock ${from} > toBlock ${to}`);
  }

  // Work queue of inclusive sub-ranges, split in half whenever the node
  // reports its pagination cap for one of them. WASM call arguments are
  // consumed by the bridge, so the tag is rebuilt per request.
  const scanTag = (): NoteTag => NoteTag.withAccountTarget(AccountId.fromHex(options.accountId));
  const queue: Array<[number, number]> = [[from, to]];
  const discovered = new Map<string, CommittedNote>();
  const uncovered: BlockRange[] = [];
  const scanReasons: string[] = [];
  let retryable = false;
  let requests = 0;
  let budgetExhausted = false;

  while (queue.length > 0) {
    const [lo, hi] = queue.shift() as [number, number];
    if (requests >= MAX_SCAN_REQUESTS) {
      budgetExhausted = true;
      uncovered.push({ from: lo, to: hi });
      continue;
    }
    requests += 1;
    try {
      const info = await retryRpcRead(() => rpcClient.syncNotes(lo, hi, [scanTag()]), rpcConfig);
      for (const committed of info.notes()) {
        const idHex = normalizeHexWord(committed.noteId().toString());
        if (!discovered.has(idHex)) {
          // The wrapper is kept (not a one-shot accessor result) so fresh
          // NoteId handles can be minted per body-fetch attempt below.
          discovered.set(idHex, committed);
        }
      }
    } catch (error) {
      // The node caps internal pagination per request rather than
      // truncating; a single-block range cannot be split further (and
      // cannot realistically hold that many pages), so only splittable
      // ranges take this branch.
      if (errorMessage(error).toLowerCase().includes(RPC_PAGINATION_FRAGMENT) && lo < hi) {
        const mid = lo + Math.floor((hi - lo) / 2);
        queue.unshift([lo, mid], [mid + 1, hi]);
        continue;
      }
      retryable ||= isTransientRpcError(error);
      scanReasons.push(`blocks [${lo}, ${hi}]: ${errorDetail(error)}`);
      uncovered.push({ from: lo, to: hi });
    }
  }
  if (budgetExhausted) {
    retryable = true;
    scanReasons.push(
      `scan budget of ${MAX_SCAN_REQUESTS} requests exhausted while splitting around the node's pagination cap; rerun the backfill over the uncovered ranges`,
    );
  }

  const publicNotes: Array<{ idHex: string; committed: CommittedNote }> = [];
  for (const [idHex, committed] of discovered) {
    if (committed.noteType() === NoteType.Public) {
      publicNotes.push({ idHex, committed });
    }
  }
  const skippedPrivate = discovered.size - publicNotes.length;

  const outcomes: NoteImportOutcome[] = [];
  const buildReport = (): PublicBackfillReport => {
    let reason: string | undefined;
    if (scanReasons.length > 0) {
      reason =
        scanReasons.length <= 3
          ? scanReasons.join('; ')
          : `${scanReasons.slice(0, 3).join('; ')}; …and ${scanReasons.length - 3} more`;
    }
    return {
      scannedFrom: from,
      scannedTo: to,
      discovered: discovered.size,
      skippedPrivate,
      outcomes,
      uncovered,
      // Rerunning can help when scan ranges were left uncovered OR when any
      // per-note outcome is itself retryable — surface both at report level
      // so orchestration keyed on the report alone reruns when it should.
      retryable: retryable || outcomes.some((outcome) => outcome.retryable === true),
      ...(reason === undefined ? {} : { reason }),
    };
  };

  interface BackfillCandidate {
    idHex: string;
    note: Note;
    proof: NoteInclusionProof;
    detailsKey: string;
  }
  const pending: BackfillCandidate[] = [];
  if (publicNotes.length > 0) {
    try {
      // One batched body fetch — the upstream client chunks internally by
      // the node's negotiated note-ids limit, and the node returns full
      // bodies for public notes, so the scan's ID + proof is all this path
      // needs. The WASM bridge consumes call arguments, so fresh NoteId
      // handles are minted from the kept wrappers on every retry attempt.
      const fetchedNotes = await retryRpcRead(
        () =>
          rpcClient.getNotesById(publicNotes.map((candidate) => candidate.committed.noteId())),
        rpcConfig,
      );
      const bodies = new Map<string, { note: Note; proof: NoteInclusionProof }>();
      for (const fetched of fetchedNotes) {
        if (fetched.note) {
          bodies.set(normalizeHexWord(fetched.noteId.toString()), {
            note: fetched.note,
            proof: fetched.inclusionProof,
          });
        }
      }
      for (const { idHex } of publicNotes) {
        const body = bodies.get(idHex);
        if (body) {
          pending.push({
            idHex,
            note: body.note,
            proof: body.proof,
            detailsKey: detailsKeyOf(
              normalizeHexWord(body.note.recipient().digest().toHex()),
              body.note.assets(),
            ),
          });
        } else {
          // Discovered as public by the scan but returned without a body —
          // not expected for a committed public note.
          outcomes.push({
            identifier: idHex,
            source: 'backfill',
            status: 'failed',
            retryable: true,
            reason: 'the node did not return a body for this public note',
          });
        }
      }
    } catch (error) {
      const fetchRetryable = isTransientRpcError(error);
      const reason = `failed to fetch note bodies: ${errorDetail(error)}`;
      for (const { idHex } of publicNotes) {
        outcomes.push({
          identifier: idHex,
          source: 'backfill',
          status: 'failed',
          retryable: fetchRetryable,
          reason,
        });
      }
    }
  }

  if (pending.length === 0) {
    return buildReport();
  }

  let existing: Map<string, InputNoteRecord>;
  try {
    existing = await collectExistingRecords(webClient);
  } catch (error) {
    const reason = `failed to read local store: ${errorDetail(error)}`;
    for (const candidate of pending) {
      outcomes.push({
        identifier: candidate.idHex,
        source: 'backfill',
        status: 'failed',
        reason,
      });
    }
    return buildReport();
  }

  // Provisionally `imported` outcomes, re-classified in one batched
  // consumed-state check below.
  const imported: Array<{ index: number; detailsKey: string }> = [];
  for (const candidate of pending) {
    const record = existing.get(candidate.idHex) ?? existing.get(candidate.detailsKey);
    // Unlike the proposal import, a proof-less (expected) record is NOT
    // skipped here: this primitive exists because forward sync will never
    // revisit the note's block, so the freshly fetched proof is applied to
    // upgrade the record in place (the WASM import handles existing
    // records).
    if (record && (record.isConsumed() || record.inclusionProof() !== undefined)) {
      outcomes.push({
        identifier: candidate.idHex,
        source: 'backfill',
        status: record.isConsumed() ? 'already-consumed' : 'already-present',
      });
      continue;
    }
    const { outcome, wasImported } = await importNoteWithProof(
      webClient,
      'backfill',
      candidate.idHex,
      candidate.note,
      candidate.proof,
    );
    if (wasImported) {
      imported.push({ index: outcomes.length, detailsKey: candidate.detailsKey });
    }
    outcomes.push(outcome);
  }

  await reclassifyConsumedImports(webClient, imported, outcomes);

  return buildReport();
}
