import type { TransactionRequest, Word } from '@miden-sdk/miden-sdk';
import {
  AccountId,
  FeltArray,
  FungibleAsset,
  MidenArrays,
  Note,
  NoteAssets,
  NoteMetadata,
  NoteRecipient,
  NoteScript,
  NoteStorage,
  NoteTag,
  NoteType,
  Poseidon2,
  TransactionRequestBuilder,
  Word as WordType,
} from '@miden-sdk/miden-sdk';
import { randomWord } from '../utils/random.js';
import { normalizeHexWord } from '../utils/encoding.js';
import type { SignatureOptions } from './options.js';
import type { P2idNoteVisibility } from '../types/proposal.js';

export interface P2idTransactionOptions extends SignatureOptions {
  /** Visibility of the created note. Defaults to `NoteType.Public`. */
  noteType?: NoteType;
}

/**
 * Parses the metadata wire value for a P2ID note visibility (issue #322).
 * Absent => public, the only behavior before the field existed. An unknown
 * value is rejected rather than silently rebuilt as a public note that could
 * never match the signed tx_summary commitment.
 */
export function parseP2idNoteType(value: string | undefined): NoteType {
  switch (value) {
    case undefined:
    case 'public':
      return NoteType.Public;
    case 'private':
      return NoteType.Private;
    default:
      throw new Error(`unsupported metadata.noteType '${value}': expected 'public' or 'private'`);
  }
}

/**
 * Maps a note type to its metadata wire value, omitting the default so
 * public-note payloads keep the pre-#322 wire shape.
 */
export function p2idNoteTypeToMetadata(noteType: NoteType | undefined): P2idNoteVisibility | undefined {
  return noteType === NoteType.Private ? 'private' : undefined;
}

export function deriveP2idSerialNumber(salt: Word): Word {
  const zeroWord = WordType.fromHex(`0x${'00'.repeat(32)}`);
  return Poseidon2.hashElements(new FeltArray([
    ...salt.toFelts(),
    ...zeroWord.toFelts(),
  ]));
}

function buildP2idNote(
  sender: AccountId,
  recipient: AccountId,
  noteAssets: NoteAssets,
  noteType: NoteType,
  saltHex: string,
): Note {
  const salt = WordType.fromHex(normalizeHexWord(saltHex));
  const serialNum = deriveP2idSerialNumber(salt);

  const noteScript = NoteScript.p2id();
  const noteStorage = new NoteStorage(new FeltArray([
    recipient.suffix(),
    recipient.prefix(),
  ]));

  const noteRecipient = new NoteRecipient(serialNum, noteScript, noteStorage);
  const noteTag = NoteTag.withAccountTarget(recipient);

  const noteMetadata = new NoteMetadata(
    sender,
    noteType,
    noteTag,
  );

  return new Note(noteAssets, noteMetadata, noteRecipient);
}

export function buildP2idTransactionRequest(
  senderId: string,
  recipientId: string,
  faucetId: string,
  amount: bigint,
  options: P2idTransactionOptions = {},
): { request: TransactionRequest; salt: Word } {
  const sender = AccountId.fromHex(senderId);
  const recipient = AccountId.fromHex(recipientId);
  const faucet = AccountId.fromHex(faucetId);

  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();

  const asset = new FungibleAsset(faucet, amount);
  const noteAssets = new NoteAssets([asset]);

  const note = buildP2idNote(
    sender,
    recipient,
    noteAssets,
    options.noteType ?? NoteType.Public,
    authSaltHex,
  );

  const outputNotes = new MidenArrays.NoteArray([note]);

  const authSaltForBuilder = WordType.fromHex(normalizeHexWord(authSaltHex));

  let txBuilder = new TransactionRequestBuilder();
  txBuilder = txBuilder.withOwnOutputNotes(outputNotes);
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
