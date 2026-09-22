import type {
  MidenClient,
  Note,
  TransactionRequest,
  WasmWebClient,
  Word,
} from '@miden-sdk/miden-sdk';
import { NoteAndArgs, NoteAndArgsArray, Word as WordType } from '@miden-sdk/miden-sdk';
import { LegacyConsumeNotesNoteMissingError } from '../multisig/consumeNotesErrors.js';
import { getRawMidenClient } from '../raw-client.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { randomWord } from '../utils/random.js';
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

  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();

  let txBuilder = await multisigRequestBuilder(client, authSaltHex, options);
  txBuilder = txBuilder.withInputNotes(noteAndArgsArray);

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
  }

  return {
    request: buildMultisigRequest(txBuilder, options.accountId),
    salt: WordType.fromHex(normalizeHexWord(authSaltHex)),
  };
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
