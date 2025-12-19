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

  console.log('[buildConsumeNotesTransactionRequest] Building transaction...');
  console.log('[buildConsumeNotesTransactionRequest] noteIds:', noteIds);
  console.log('[buildConsumeNotesTransactionRequest] options.salt:', !!options.salt);
  console.log('[buildConsumeNotesTransactionRequest] options.signatureAdviceMap:', !!options.signatureAdviceMap);

  const noteIdAndArgsArray = new NoteIdAndArgsArray();
  for (const noteIdHex of noteIds) {
    const noteId = NoteId.fromHex(noteIdHex);
    const noteIdAndArgs = new NoteIdAndArgs(noteId, null);
    noteIdAndArgsArray.push(noteIdAndArgs);
  }
  console.log('[buildConsumeNotesTransactionRequest] Created noteIdAndArgsArray');

  // Store salt as hex so we can create fresh Word instances (WASM objects get consumed)
  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();
  console.log('[buildConsumeNotesTransactionRequest] authSaltHex:', authSaltHex);

  // Create fresh Word for withAuthArg
  const authSaltForBuilder = WordType.fromHex(normalizeHexWord(authSaltHex));
  console.log('[buildConsumeNotesTransactionRequest] Created authSaltForBuilder');

  let txBuilder = new TransactionRequestBuilder();
  console.log('[buildConsumeNotesTransactionRequest] Created builder');

  txBuilder = txBuilder.withAuthenticatedInputNotes(noteIdAndArgsArray);
  console.log('[buildConsumeNotesTransactionRequest] Added authenticated input notes');

  txBuilder = txBuilder.withAuthArg(authSaltForBuilder);
  console.log('[buildConsumeNotesTransactionRequest] Added auth arg');

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
    console.log('[buildConsumeNotesTransactionRequest] Extended advice map (signatures)');
  }

  console.log('[buildConsumeNotesTransactionRequest] About to build...');

  // Create fresh Word for return value
  const authSaltForReturn = WordType.fromHex(normalizeHexWord(authSaltHex));

  const request = txBuilder.build();
  console.log('[buildConsumeNotesTransactionRequest] Build successful');

  return {
    request,
    salt: authSaltForReturn,
  };
}

