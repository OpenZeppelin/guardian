import type {
  MidenClient,
  Note,
  TransactionRequest,
  WasmWebClient,
  Word,
} from '@miden-sdk/miden-sdk';
import { InputNote, NoteAndArgs, NoteAndArgsArray } from '@miden-sdk/miden-sdk';
import { ConsumeNoteNotAuthenticatedError } from '../multisig/consumeNotesErrors.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { LegacyConsumeNotesNoteMissingError } from '../multisig/consumeNotesErrors.js';
import { getRawMidenClient } from '../raw-client.js';
import { buildMultisigRequest, multisigRequestBuilder } from './authArgs.js';
import type { MidenClientMultisigRequestOptions, MultisigRequestOptions } from './options.js';

/**
 * Build a consume-notes request from loaded `Note` objects (no local-store
 * read). v2 verification path for issue #229.
 */
export function buildConsumeNotesTransactionRequestFromNotes(
  client: MidenClient,
  notes: Note[],
  options: MidenClientMultisigRequestOptions,
): Promise<{ request: TransactionRequest; salt: Word }>;
export function buildConsumeNotesTransactionRequestFromNotes(
  client: WasmWebClient,
  notes: Note[],
  options: MultisigRequestOptions,
): Promise<{ request: TransactionRequest; salt: Word }>;
export async function buildConsumeNotesTransactionRequestFromNotes(
  client: MidenClient | WasmWebClient,
  notes: Note[],
  options: MultisigRequestOptions,
): Promise<{ request: TransactionRequest; salt: Word }> {
  if (notes.length === 0) {
    throw new Error('At least one note is required');
  }

  const noteAndArgsArray = new NoteAndArgsArray();
  for (const note of notes) {
    noteAndArgsArray.push(new NoteAndArgs(note, null));
  }

  const { builder, saltHex } = await multisigRequestBuilder(client, options);
  let txBuilder = builder.withInputNotes(noteAndArgsArray);
  if (options.transactionExpirationDelta) {
    txBuilder = txBuilder.withExpirationDelta(options.transactionExpirationDelta);
  }

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
  }

  return buildMultisigRequest(txBuilder, saltHex, options.accountId);
}

/**
 * Legacy/creation adapter: fetches notes from the local store and delegates
 * to the from-notes variant. v2 verification MUST NOT call this.
 */
export function buildConsumeNotesTransactionRequest(
  client: MidenClient,
  noteIds: string[],
  options: MidenClientMultisigRequestOptions,
): Promise<{ request: TransactionRequest; salt: Word }>;
export function buildConsumeNotesTransactionRequest(
  client: WasmWebClient,
  noteIds: string[],
  options: MultisigRequestOptions,
): Promise<{ request: TransactionRequest; salt: Word }>;
export async function buildConsumeNotesTransactionRequest(
  client: MidenClient | WasmWebClient,
  noteIds: string[],
  options: MultisigRequestOptions,
): Promise<{ request: TransactionRequest; salt: Word }> {
  if (noteIds.length === 0) {
    throw new Error('At least one note ID is required');
  }

  const rawClient = await getRawMidenClient(client, options.midenRpcEndpoint);
  const notes: Note[] = [];
  for (const noteIdHex of noteIds) {
    const inputNoteRecord = await rawClient.getInputNote(noteIdHex);
    if (!inputNoteRecord) {
      throw new LegacyConsumeNotesNoteMissingError(noteIdHex);
    }
    notes.push(inputNoteRecord.toNote());
  }

  return buildConsumeNotesTransactionRequestFromNotes(rawClient, notes, options);
}

/**
 * A consume-notes request whose notes are pinned as authenticated, with their inclusion proofs,
 * so any party executes it in the same mode without consulting its own store. This is the
 * request a Guardian-executable proposal stores; authenticate the notes first.
 */
export async function buildPinnedConsumeNotesTransactionRequest(
  client: WasmWebClient,
  notes: Note[],
  options: MultisigRequestOptions,
): Promise<{ request: TransactionRequest; salt: Word }> {
  if (notes.length === 0) {
    throw new Error('At least one note is required');
  }
  const { builder, saltHex } = await multisigRequestBuilder(client, options);
  let txBuilder = builder;
  for (const note of notes) {
    const noteId = normalizeHexWord(note.id().toString());
    const record = await client.getInputNote(noteId);
    const proof = record?.inclusionProof();
    if (!proof) {
      throw new ConsumeNoteNotAuthenticatedError(
        noteId,
        'the local store holds no inclusion proof to pin it with',
      );
    }
    txBuilder = txBuilder.withExplicitInputNote(InputNote.authenticated(note, proof), null);
  }
  if (options.transactionExpirationDelta) {
    txBuilder = txBuilder.withExpirationDelta(options.transactionExpirationDelta);
  }
  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
  }
  return buildMultisigRequest(txBuilder, saltHex, options.accountId);
}
