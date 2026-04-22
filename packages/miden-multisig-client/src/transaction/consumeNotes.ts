import type {
  MidenClient,
  TransactionRequest,
  WasmWebClient,
  Word,
} from '@miden-sdk/miden-sdk';
import {
  NoteAndArgs,
  NoteAndArgsArray,
  TransactionRequestBuilder,
  Word as WordType,
} from '@miden-sdk/miden-sdk';
import { getRawMidenClient } from '../raw-client.js';
import { randomWord } from '../utils/random.js';
import { normalizeHexWord } from '../utils/encoding.js';
import type { SignatureOptions } from './options.js';

export async function buildConsumeNotesTransactionRequest(
  client: MidenClient | WasmWebClient,
  noteIds: string[],
  options: SignatureOptions = {},
): Promise<{ request: TransactionRequest; salt: Word }> {
  if (noteIds.length === 0) {
    throw new Error('At least one note ID is required');
  }

  const rawClient = await getRawMidenClient(client, options.midenRpcEndpoint);
  const noteAndArgsArray = new NoteAndArgsArray();
  for (const noteIdHex of noteIds) {
    const inputNoteRecord = await rawClient.getInputNote(noteIdHex);
    if (!inputNoteRecord) {
      throw new Error(`Note not found in local store: ${noteIdHex}`);
    }
    const note = inputNoteRecord.toNote();
    const noteAndArgs = new NoteAndArgs(note, null);
    noteAndArgsArray.push(noteAndArgs);
  }

  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();

  const authSaltForBuilder = WordType.fromHex(normalizeHexWord(authSaltHex));

  let txBuilder = new TransactionRequestBuilder();
  txBuilder = txBuilder.withInputNotes(noteAndArgsArray);
  txBuilder = txBuilder.withAuthArg(authSaltForBuilder);

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
  }

  const authSaltForReturn = WordType.fromHex(normalizeHexWord(authSaltHex));

  return {
    request: txBuilder.build(),
    salt: authSaltForReturn,
  };
}
