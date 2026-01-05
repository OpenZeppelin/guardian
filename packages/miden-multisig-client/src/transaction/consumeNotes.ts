import type { TransactionRequest, Word } from '@demox-labs/miden-sdk';
import { NoteId, NoteIdAndArgs, NoteIdAndArgsArray, TransactionRequestBuilder, Word as WordType } from '@demox-labs/miden-sdk';
import { randomWord } from '../utils/random.js';
import { normalizeHexWord } from '../utils/encoding.js';
import type { SignatureOptions } from './options.js';

export function buildConsumeNotesTransactionRequest(
  noteIds: string[],
  options: SignatureOptions = {},
): { request: TransactionRequest; salt: Word } {
  if (noteIds.length === 0) {
    throw new Error('At least one note ID is required');
  }

  const noteIdAndArgsArray = new NoteIdAndArgsArray();
  for (const noteIdHex of noteIds) {
    const noteId = NoteId.fromHex(noteIdHex);
    const noteIdAndArgs = new NoteIdAndArgs(noteId, null);
    noteIdAndArgsArray.push(noteIdAndArgs);
  }

  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();

  const authSaltForBuilder = WordType.fromHex(normalizeHexWord(authSaltHex));

  let txBuilder = new TransactionRequestBuilder();
  txBuilder = txBuilder.withAuthenticatedInputNotes(noteIdAndArgsArray);
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

