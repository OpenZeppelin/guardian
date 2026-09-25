/**
 * Canonical consumption mode for `consume_notes` proposals: authenticated.
 *
 * miden-client decides per input note, at execution time and from the local
 * store alone, whether it is consumed as *authenticated* (the store holds
 * its inclusion proof) or *unauthenticated* (anything else). The two modes
 * commit differently into the transaction summary,
 * `hash(nullifier || note_id_or_ZERO)`, so the proposer and every verifier
 * must be in the same mode or the summary commitment, and with it the
 * proposal id, differs. That was issue #409's live shape: a cosigner whose
 * fresh store had never seen the notes failed with "metadata does not match
 * tx_summary". Proposal creation and every rebuild call
 * {@link ensureNotesAuthenticated} first, so the executed transaction is the
 * one the cosigners signed regardless of what each store held before.
 */
import { Endpoint, type Note, type NoteInclusionProof, RpcClient } from '@miden-sdk/miden-sdk';

import { ConsumeNoteNotAuthenticatedError } from '../multisig/consumeNotesErrors.js';
import type { RawClientSource } from '../raw-client.js';
import { getRawMidenClient } from '../raw-client.js';
import { importNoteWithProof } from '../recovery/proposalNoteImport.js';
import { resolveRpcConfig, type RpcConfig } from '../rpc/config.js';
import { retryRpcRead } from '../rpc/retry.js';
import { normalizeHexWord } from '../utils/encoding.js';

export interface EnsureNotesAuthenticatedOptions {
  /** Miden node RPC endpoint the inclusion proofs are fetched from. */
  midenRpcEndpoint: string;
  /** Retry policy for the proof fetch; defaults to the SDK's RPC defaults. */
  rpc?: RpcConfig;
}

/**
 * Makes every note in `notes` an authenticated input note in the client's
 * local store: present, with its on-chain inclusion proof and block header.
 *
 * Notes already authenticated locally are left alone. For the rest the
 * inclusion proofs come from the node in one round trip (the node serves
 * proofs for private notes too) and each note is imported as committed,
 * which also upgrades a proof-less record the store already tracked.
 *
 * @throws {ConsumeNoteNotAuthenticatedError} when a note is not committed on
 *   chain yet, the node does not serve its proof, the import fails, or the
 *   record still lacks authentication after a sync.
 */
export async function ensureNotesAuthenticated(
  midenClient: RawClientSource,
  notes: readonly Note[],
  options: EnsureNotesAuthenticatedOptions,
): Promise<void> {
  const webClient = await getRawMidenClient(midenClient);
  const pending = await unauthenticatedNotes(webClient, notes);
  if (pending.length === 0) {
    return;
  }

  const proofs = new Map<string, NoteInclusionProof>();
  try {
    const rpcClient = new RpcClient(new Endpoint(options.midenRpcEndpoint));
    const fetched = await retryRpcRead(
      () => rpcClient.getNotesById(pending.map((note) => note.id())),
      resolveRpcConfig(options.rpc),
    );
    for (const entry of fetched) {
      proofs.set(normalizeHexWord(entry.noteId.toString()), entry.inclusionProof);
    }
  } catch (error) {
    throw new ConsumeNoteNotAuthenticatedError(
      noteIdHex(pending[0]),
      `failed to fetch inclusion proofs from the node: ${errorDetail(error)}`,
    );
  }

  for (const note of pending) {
    const idHex = noteIdHex(note);
    const proof = proofs.get(idHex);
    if (!proof) {
      throw new ConsumeNoteNotAuthenticatedError(
        idHex,
        'the node has no inclusion proof for it (not committed on chain yet)',
      );
    }
    const { outcome, wasImported } = await importNoteWithProof(
      webClient,
      'proposal',
      idHex,
      note,
      proof,
    );
    if (!wasImported) {
      throw new ConsumeNoteNotAuthenticatedError(
        idHex,
        `failed to import it with its inclusion proof: ${outcome.reason ?? 'unknown error'}`,
      );
    }
  }

  // The import authenticates a note committed at or below the client's sync
  // height; a newer one lands unverified until a sync fetches its block
  // header. A cosigner that just loaded the account is typically behind the
  // note's block, so sync once and re-check before failing.
  if ((await unauthenticatedNotes(webClient, notes)).length > 0) {
    await webClient.syncState();
  }
  const still = await unauthenticatedNotes(webClient, notes);
  if (still.length > 0) {
    throw new ConsumeNoteNotAuthenticatedError(
      noteIdHex(still[0]),
      'its inclusion proof was imported but the local store could not verify it ' +
        'against the chain even after a sync',
    );
  }
}

/** The subset of `notes` whose local record is missing or carries no proof. */
async function unauthenticatedNotes(
  webClient: Awaited<ReturnType<typeof getRawMidenClient>>,
  notes: readonly Note[],
): Promise<Note[]> {
  const pending: Note[] = [];
  for (const note of notes) {
    const record = await webClient.getInputNote(noteIdHex(note));
    if (!record || !record.isAuthenticated()) {
      pending.push(note);
    }
  }
  return pending;
}

function noteIdHex(note: Note): string {
  return normalizeHexWord(note.id().toString());
}

function errorDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
