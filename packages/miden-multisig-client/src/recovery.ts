/**
 * Recovery primitives for restoring note state after device loss.
 *
 * After key-based recovery the local Miden store starts empty — and in a
 * shared dirty store another account's sync may have advanced cursors past
 * notes belonging to the newly recovered account. The primitives here rescan
 * sources that normal forward sync would skip: the private-note transport
 * backlog (issue #414) and the notes embedded in v2 `consume_notes`
 * proposals (issue #415) — sub-issues of #357.
 */

import {
  Endpoint,
  InputNote,
  type InputNoteRecord,
  type MidenClient,
  Note,
  type NoteAssets,
  NoteDetails,
  NoteFile,
  NoteFilter,
  NoteFilterTypes,
  type NoteInclusionProof,
  RpcClient,
} from '@miden-sdk/miden-sdk';

import { errorMessage, isLikelyNetworkError } from './connectivity.js';
import { getRawMidenClient, requireMidenRpcEndpoint, type RawClientSource } from './raw-client.js';
import { resolveRpcConfig, type RpcConfig } from './rpc/config.js';
import { isTransientRpcError } from './rpc/errors.js';
import { retryRpcRead } from './rpc/retry.js';
import { isConsumeNotesV2 } from './types/proposal.js';
import type { Proposal } from './types/proposal.js';
import { noteFromBase64, normalizeHexWord } from './utils/encoding.js';

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

/** Where a recovered note's bytes came from. */
export type NoteImportSource = 'proposal';

/** Per-note result of a recovery import attempt. */
export type NoteImportStatus =
  /** Note imported with its on-chain inclusion proof; it lands in the store's
   * unverified state and the next sync verifies it. */
  | 'imported'
  /** The local store already tracks this note (not yet consumed). */
  | 'already-present'
  /** The note is already consumed — either the local store tracked it as
   * consumed, or the chain had nullified it and the import recorded it as
   * consumption history rather than a consumable note. */
  | 'already-consumed'
  /** The chain does not know the note yet. Its details were recorded as
   * expected with its tag tracked, so a later sync picks it up once it
   * commits. */
  | 'not-committed'
  /** The embedded bytes could not be decoded into a note. */
  | 'invalid'
  /** The import attempt failed (store or RPC error). */
  | 'failed';

/**
 * Outcome of one unique embedded note's recovery import. A batch of outcomes
 * is the full report of {@link importNotesFromProposals}; no per-note problem
 * aborts the batch. A note embedded by several proposals is deduplicated into
 * a single outcome (its first occurrence).
 */
export interface NoteImportOutcome {
  /** The note ID hex when the bytes decoded, otherwise a positional reference
   * into the proposal (`proposal <id> notes[<i>]`). */
  identifier: string;
  /** Where the note bytes came from. */
  source: NoteImportSource;
  /** What happened to this note. */
  status: NoteImportStatus;
  /** Whether retrying the import later can change the status (transient RPC
   * failures, notes not yet committed). Absent means not retryable. */
  retryable?: boolean;
  /** Human-readable detail for non-success statuses — or, on an `imported`
   * outcome, a warning that the post-import consumed-state check failed and a
   * sync should confirm the note's status. */
  reason?: string;
}

export interface ImportNotesFromProposalsOptions {
  /** Miden node RPC endpoint used to fetch inclusion proofs. Must point at
   * the same network as the injected Miden client. */
  midenRpcEndpoint: string;
  /** Node RPC read-retry configuration (defaults match the rest of the SDK). */
  rpc?: RpcConfig;
}

function errorDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

interface DecodedCandidate {
  note: Note;
  idHex: string;
  /** Metadata-independent identifier: metadata-less store records (details
   * imports in expected state, chain-consumed history) expose neither a note
   * ID nor a nullifier, so records are matched by their details — recipient
   * digest plus asset fingerprint, together equivalent to the details
   * commitment (recipient alone is not collision-safe: two distinct notes
   * can share a recipient while carrying different assets). */
  detailsKey: string;
  /** Decimal string of the note's tag, for `addTag`. */
  tagString: string;
}

/** Collision-safe key over full note details: recipient digest + canonical
 * asset list. Mirrors what the details commitment covers, which the WASM
 * record surface does not expose directly. */
function detailsKeyOf(recipientDigestHex: string, assets: NoteAssets): string {
  const fingerprint = assets
    .fungibleAssets()
    .map((asset) => `${normalizeHexWord(asset.faucetId().toString())}:${asset.amount()}`)
    .sort()
    .join(',');
  return `${recipientDigestHex}|${fingerprint}`;
}

function recordKeys(record: InputNoteRecord): string[] {
  const keys: string[] = [];
  const recordId = record.id();
  if (recordId) {
    keys.push(normalizeHexWord(recordId.toString()));
  }
  const details = record.details();
  keys.push(
    detailsKeyOf(normalizeHexWord(details.recipient().digest().toHex()), details.assets()),
  );
  return keys;
}

/**
 * Imports the notes embedded in v2 `consume_notes` proposals into the local
 * Miden store (issue #415), typically after key-based recovery rebuilt the
 * proposal list (`syncProposals`) but left the note store empty.
 *
 * Proposals are opportunistic recovery material, not a backup: v1 proposals
 * carry no note bytes, and proposals disappear once canonicalized, so only
 * notes still mid-consumption are recoverable this way.
 *
 * Per note: decode the embedded bytes, skip notes the store already tracks,
 * fetch the on-chain inclusion proof, and import the note individually
 * (upstream note-import batches are atomic, so one bad note must not sink the
 * rest). A note the chain does not know yet is recorded as expected with its
 * tag tracked so a later sync picks it up, and is reported as
 * `not-committed`/retryable. A note the chain has already nullified is
 * recorded as consumption history and reported `already-consumed`.
 *
 * The returned outcomes cover every unique embedded note — a note embedded by
 * several proposals yields one outcome, not one per embedding — and this
 * function does not throw for per-note problems.
 *
 * @example
 * ```typescript
 * const proposals = await multisig.syncProposals();
 * const outcomes = await importNotesFromProposals(midenClient, proposals, {
 *   midenRpcEndpoint: 'https://rpc.testnet.miden.io',
 * });
 * await multisig.syncState();
 * ```
 */
export async function importNotesFromProposals(
  midenClient: RawClientSource,
  proposals: ReadonlyArray<Pick<Proposal, 'id' | 'metadata'>>,
  options: ImportNotesFromProposalsOptions,
): Promise<NoteImportOutcome[]> {
  const midenRpcEndpoint = requireMidenRpcEndpoint(options.midenRpcEndpoint);
  const rpcConfig = resolveRpcConfig(options.rpc);
  const webClient = await getRawMidenClient(midenClient, midenRpcEndpoint);

  const outcomes: NoteImportOutcome[] = [];

  // Decode and deduplicate embedded notes (the same note may be embedded by
  // several proposals); undecodable entries become isolated `invalid`
  // outcomes with a positional identifier. Decoding is deliberately
  // permissive: the strict note-ID binding check belongs to the
  // verify/execute path — a note is self-validating (its ID derives from its
  // contents), so importing it is harmless even when the proposal's declared
  // note IDs disagree.
  const decoded: DecodedCandidate[] = [];
  const seen = new Set<string>();
  for (const proposal of proposals) {
    const metadata = proposal.metadata;
    if (metadata.proposalType !== 'consume_notes' || !isConsumeNotesV2(metadata)) {
      continue;
    }
    const embedded = metadata.notes ?? [];
    for (let index = 0; index < embedded.length; index += 1) {
      // The try covers every per-note WASM accessor, so a payload that
      // deserializes but traps on use is isolated like any other bad note.
      let candidate: DecodedCandidate;
      try {
        const note = noteFromBase64(embedded[index], Note);
        candidate = {
          note,
          idHex: normalizeHexWord(note.id().toString()),
          detailsKey: detailsKeyOf(
            normalizeHexWord(note.recipient().digest().toHex()),
            note.assets(),
          ),
          tagString: String(note.metadata().tag().asU32()),
        };
      } catch (error) {
        outcomes.push({
          identifier: `proposal ${proposal.id} notes[${index}]`,
          source: 'proposal',
          status: 'invalid',
          reason: `failed to decode embedded note: ${errorDetail(error)}`,
        });
        continue;
      }
      if (seen.has(candidate.idHex)) {
        continue;
      }
      seen.add(candidate.idHex);
      decoded.push(candidate);
    }
  }

  if (decoded.length === 0) {
    return outcomes;
  }

  // Look up existing records by note ID *and* recipient digest: records the
  // store keeps without metadata (a note details import in expected state, or
  // a note observed as consumed on chain) expose neither a note ID nor a
  // nullifier, and an ID-only lookup would keep re-importing them forever.
  // (The WASM NoteFilter has no details-commitment variant, unlike the Rust
  // SDK, so this scans the store once and keys records both ways.)
  const existing = new Map<string, InputNoteRecord>();
  try {
    const records = await webClient.getInputNotes(new NoteFilter(NoteFilterTypes.All));
    for (const record of records) {
      for (const key of recordKeys(record)) {
        existing.set(key, record);
      }
    }
  } catch (error) {
    const reason = `failed to read local store: ${errorDetail(error)}`;
    for (const candidate of decoded) {
      outcomes.push({
        identifier: candidate.idHex,
        source: 'proposal',
        status: 'failed',
        reason,
      });
    }
    return outcomes;
  }

  // Skip notes the store already tracks.
  const pending: DecodedCandidate[] = [];
  for (const candidate of decoded) {
    const record = existing.get(candidate.idHex) ?? existing.get(candidate.detailsKey);
    if (record) {
      outcomes.push({
        identifier: candidate.idHex,
        source: 'proposal',
        status: record.isConsumed() ? 'already-consumed' : 'already-present',
      });
      continue;
    }
    pending.push(candidate);
  }

  if (pending.length === 0) {
    return outcomes;
  }

  // One round trip for all missing notes; only the import itself is per-note.
  // The node returns proofs for private notes too, so the locally-held bytes
  // are the only body this path ever needs.
  const proofs = new Map<string, NoteInclusionProof>();
  try {
    const rpcClient = new RpcClient(new Endpoint(midenRpcEndpoint));
    const fetchedNotes = await retryRpcRead(
      () => rpcClient.getNotesById(pending.map((candidate) => candidate.note.id())),
      rpcConfig,
    );
    for (const fetched of fetchedNotes) {
      proofs.set(normalizeHexWord(fetched.noteId.toString()), fetched.inclusionProof);
    }
  } catch (error) {
    const retryable = isTransientRpcError(error);
    const reason = `failed to fetch inclusion proofs: ${errorDetail(error)}`;
    for (const candidate of pending) {
      outcomes.push({
        identifier: candidate.idHex,
        source: 'proposal',
        status: 'failed',
        retryable,
        reason,
      });
    }
    return outcomes;
  }

  // Provisionally `imported` outcomes, re-classified in one batched
  // consumed-state check below.
  const imported: Array<{ index: number; detailsKey: string }> = [];

  for (const candidate of pending) {
    const proof = proofs.get(candidate.idHex);
    if (proof) {
      try {
        const inputNote = InputNote.authenticated(candidate.note, proof);
        await webClient.importNoteFile(NoteFile.fromInputNote(inputNote));
        imported.push({ index: outcomes.length, detailsKey: candidate.detailsKey });
        outcomes.push({
          identifier: candidate.idHex,
          source: 'proposal',
          status: 'imported',
        });
      } catch (error) {
        outcomes.push({
          identifier: candidate.idHex,
          source: 'proposal',
          status: 'failed',
          retryable: isTransientRpcError(error),
          reason: `failed to import note: ${errorDetail(error)}`,
        });
      }
    } else {
      try {
        // Track the tag FIRST so the resulting expected record can never
        // exist untagged: the WASM details import cannot carry a tag (unlike
        // the Rust SDK's note file), and sync only discovers the note's
        // commitment through a tracked tag. Tag first also means a failure
        // here leaves no dead record behind.
        await webClient.addTag(candidate.tagString);
        const details = new NoteDetails(candidate.note.assets(), candidate.note.recipient());
        await webClient.importNoteFile(NoteFile.fromNoteDetails(details));
        outcomes.push({
          identifier: candidate.idHex,
          source: 'proposal',
          status: 'not-committed',
          retryable: true,
          reason: 'note not yet committed on chain; recorded as expected so a later sync picks it up',
        });
      } catch (error) {
        outcomes.push({
          identifier: candidate.idHex,
          source: 'proposal',
          status: 'failed',
          retryable: isTransientRpcError(error),
          reason: `failed to record expected note: ${errorDetail(error)}`,
        });
      }
    }
  }

  // A note the chain already nullified is stored as consumption history, not
  // as a consumable note — report that honestly instead of `imported`. One
  // batched store read covers every imported note.
  if (imported.length > 0) {
    try {
      const consumedRecords = await webClient.getInputNotes(
        new NoteFilter(NoteFilterTypes.Consumed),
      );
      const consumed = new Set(
        consumedRecords.map((record) => {
          const details = record.details();
          return detailsKeyOf(
            normalizeHexWord(details.recipient().digest().toHex()),
            details.assets(),
          );
        }),
      );
      for (const entry of imported) {
        if (consumed.has(entry.detailsKey)) {
          outcomes[entry.index] = {
            ...outcomes[entry.index],
            status: 'already-consumed',
            reason: 'note was already consumed on chain; recorded as consumption history',
          };
        }
      }
    } catch (error) {
      // The imports themselves succeeded; stay `imported` but flag that the
      // consumed-state classification is unknown.
      for (const entry of imported) {
        outcomes[entry.index] = {
          ...outcomes[entry.index],
          reason: `imported, but the consumed-state check failed (${errorDetail(
            error,
          )}); run sync to confirm the note's status`,
        };
      }
    }
  }

  return outcomes;
}
