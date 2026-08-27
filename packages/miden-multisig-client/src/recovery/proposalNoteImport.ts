/**
 * Recovery primitives.
 *
 * After key-based recovery the local Miden store starts empty, so notes the
 * account was in the middle of consuming are gone. v2 `consume_notes`
 * proposals embed the serialized notes they consume, which makes
 * pending proposals opportunistic recovery material: this module rebuilds
 * importable notes from those embedded bytes plus a node-fetched inclusion
 * proof, without needing the node to hold the note body — so it works for
 * private notes too.
 */

import {
  Endpoint,
  InputNote,
  type InputNoteRecord,
  Note,
  type NoteAssets,
  NoteDetails,
  NoteFile,
  NoteFilter,
  NoteFilterTypes,
  type NoteInclusionProof,
  RpcClient,
} from '@miden-sdk/miden-sdk';

import { getRawMidenClient, requireMidenRpcEndpoint, type RawClientSource } from '../raw-client.js';
import { resolveRpcConfig, type RpcConfig } from '../rpc/config.js';
import { isTransientRpcError } from '../rpc/errors.js';
import { retryRpcRead } from '../rpc/retry.js';
import { isConsumeNotesV2 } from '../types/proposal.js';
import type { Proposal } from '../types/proposal.js';
import { noteFromBase64, normalizeHexWord } from '../utils/encoding.js';

/** Where a recovered note's bytes came from. */
export type NoteImportSource =
  /** Embedded in a v2 `consume_notes` proposal. */
  | 'proposal'
  /** Discovered on chain by a tag-scoped historical scan
   * (`backfillPublicNotesByTag`). */
  | 'backfill';

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

/** Shared by the recovery primitives (proposal import and backfill). */
export function errorDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

interface DecodedCandidate {
  note: Note;
  idHex: string;
  /** Metadata-independent identifier: metadata-less store records (details
   * imports in expected state, chain-consumed history) expose neither a note
   * ID nor a nullifier, so records are matched by their details — recipient
   * digest plus asset fingerprint (see {@link detailsKeyOf} for the
   * fingerprint's coverage and its limits). */
  detailsKey: string;
}

/** Details key: recipient digest + canonical FUNGIBLE asset list. This
 * approximates the details commitment — which the WASM record surface does
 * not expose — as closely as the surface allows: `fungibleAssets()` silently
 * omits non-fungible assets and no complete accessor or commitment exists,
 * so two notes sharing a recipient digest and fungible assets but differing
 * only in non-fungible assets collide (the Rust SDK, keyed on the real
 * `NoteDetailsCommitment`, does not). Latent until NFA-bearing notes reach
 * these flows; closing it needs an upstream `NoteAssets` commitment/NFA
 * accessor. Recipient digest alone would be worse: distinct notes can share
 * a recipient while carrying different fungible assets. */
export function detailsKeyOf(recipientDigestHex: string, assets: NoteAssets): string {
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
 * Scans the store once and keys every record by note ID *and* details key:
 * records the store keeps without metadata (a note details import in
 * expected state, or a note observed as consumed on chain) expose neither a
 * note ID nor a nullifier, and an ID-only lookup would keep re-importing
 * them forever. (The WASM NoteFilter has no details-commitment variant,
 * unlike the Rust SDK, so the store is scanned once and keyed both ways.)
 * Shared by the recovery primitives (proposal import and backfill).
 */
export async function collectExistingRecords(
  webClient: Awaited<ReturnType<typeof getRawMidenClient>>,
): Promise<Map<string, InputNoteRecord>> {
  const existing = new Map<string, InputNoteRecord>();
  const records = await webClient.getInputNotes(new NoteFilter(NoteFilterTypes.All));
  for (const record of records) {
    for (const key of recordKeys(record)) {
      existing.set(key, record);
    }
  }
  return existing;
}

/**
 * Imports one note with its inclusion proof and classifies the result.
 * Upstream note-import batches are atomic, which is why callers import
 * individually — one bad note must not sink the rest. Returns the outcome
 * and whether the import succeeded (input for the batched consumed-state
 * re-check).
 */
export async function importNoteWithProof(
  webClient: Awaited<ReturnType<typeof getRawMidenClient>>,
  source: NoteImportSource,
  idHex: string,
  note: Note,
  proof: NoteInclusionProof,
): Promise<{ outcome: NoteImportOutcome; wasImported: boolean }> {
  try {
    const inputNote = InputNote.authenticated(note, proof);
    await webClient.importNoteFile(NoteFile.fromInputNote(inputNote));
    return {
      outcome: { identifier: idHex, source, status: 'imported' },
      wasImported: true,
    };
  } catch (error) {
    return {
      outcome: {
        identifier: idHex,
        source,
        status: 'failed',
        retryable: isTransientRpcError(error),
        reason: `failed to import note: ${errorDetail(error)}`,
      },
      wasImported: false,
    };
  }
}

/**
 * Re-classifies provisionally `imported` outcomes whose note the chain had
 * already nullified: upstream stores those as consumption history, not as
 * consumable notes — report that honestly instead of `imported`. One batched
 * store read covers every imported note. A failed check downgrades nothing;
 * it flags the outcome's classification as unconfirmed instead.
 */
export async function reclassifyConsumedImports(
  webClient: Awaited<ReturnType<typeof getRawMidenClient>>,
  imported: Array<{ index: number; detailsKey: string }>,
  outcomes: NoteImportOutcome[],
): Promise<void> {
  if (imported.length === 0) {
    return;
  }
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

/**
 * Imports the notes embedded in v2 `consume_notes` proposals into the local
 * Miden store, typically after key-based recovery rebuilt the
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

  let existing: Map<string, InputNoteRecord>;
  try {
    existing = await collectExistingRecords(webClient);
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
      const { outcome, wasImported } = await importNoteWithProof(
        webClient,
        'proposal',
        candidate.idHex,
        candidate.note,
        proof,
      );
      if (wasImported) {
        imported.push({ index: outcomes.length, detailsKey: candidate.detailsKey });
      }
      outcomes.push(outcome);
    } else {
      try {
        // Mirror the Rust SDK's `NoteFile::ExpectedNote` + sync-hint import:
        // the tag rides in the note file itself, so upstream registers a
        // note-source tag that sync uses to discover the commitment and
        // removes once the note commits. (An explicit `addTag` would instead
        // create a permanent user-source tag that the transport backfill
        // also re-drains, and that would outlive even a failed import.)
        const details = new NoteDetails(candidate.note.assets(), candidate.note.recipient());
        await webClient.importNoteFile(
          NoteFile.fromExpectedNote(details, candidate.note.metadata().tag(), 0),
        );
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

  await reclassifyConsumedImports(webClient, imported, outcomes);

  return outcomes;
}
