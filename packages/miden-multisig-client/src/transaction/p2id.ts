import type { TransactionRequest, Word, AdviceMap } from '@demox-labs/miden-sdk';
import {
  AccountId,
  Felt,
  FeltArray,
  FungibleAsset,
  MidenArrays,
  Note,
  NoteAssets,
  NoteExecutionHint,
  NoteInputs,
  NoteMetadata,
  NoteRecipient,
  NoteScript,
  NoteTag,
  NoteType,
  OutputNote,
  Rpo256,
  TransactionRequestBuilder,
  Word as WordType,
} from '@demox-labs/miden-sdk';
import { randomWord } from '../utils/random.js';
import { normalizeHexWord } from '../utils/encoding.js';
import type { SignatureOptions } from './options.js';

function buildP2idNote(
  sender: AccountId,
  recipient: AccountId,
  noteAssets: NoteAssets,
  noteType: NoteType,
  aux: Felt,
  saltHex: string,
): Note {
  // Create salt from hex (WASM objects get consumed when used)
  const salt = WordType.fromHex(normalizeHexWord(saltHex));
  const serialNum = Rpo256.hashElements(new FeltArray([
    ...salt.toFelts(),
    new Felt(0n),
  ]));

  const noteScript = NoteScript.p2id();
  const noteInputs = new NoteInputs(new FeltArray([
    recipient.suffix(),
    recipient.prefix(),
  ]));

  const noteRecipient = new NoteRecipient(serialNum, noteScript, noteInputs);
  const noteTag = NoteTag.fromAccountId(recipient);

  const noteMetadata = new NoteMetadata(
    sender,
    noteType,
    noteTag,
    NoteExecutionHint.always(),
    aux,
  );

  return new Note(noteAssets, noteMetadata, noteRecipient);
}

export function buildP2idTransactionRequest(
  senderId: string,
  recipientId: string,
  faucetId: string,
  amount: bigint,
  options: SignatureOptions = {},
): { request: TransactionRequest; salt: Word } {
  const sender = AccountId.fromHex(senderId);
  const recipient = AccountId.fromHex(recipientId);
  const faucet = AccountId.fromHex(faucetId);

  // Store salt as hex so we can create fresh Word instances (WASM objects get consumed)
  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();

  const asset = new FungibleAsset(faucet, amount);
  const noteAssets = new NoteAssets([asset]);

  // buildP2idNote will create its own Word from the hex
  const note = buildP2idNote(
    sender,
    recipient,
    noteAssets,
    NoteType.Public,
    new Felt(0n),
    authSaltHex,
  );

  const outputNote = OutputNote.full(note);
  const outputNotes = new MidenArrays.OutputNoteArray([outputNote]);

  // Create fresh Word for withAuthArg
  const authSaltForBuilder = WordType.fromHex(normalizeHexWord(authSaltHex));

  let txBuilder = new TransactionRequestBuilder();
  txBuilder = txBuilder.withOwnOutputNotes(outputNotes);
  txBuilder = txBuilder.withAuthArg(authSaltForBuilder);

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
  }

  // Create fresh Word for return value
  const authSaltForReturn = WordType.fromHex(normalizeHexWord(authSaltHex));

  return {
    request: txBuilder.build(),
    salt: authSaltForReturn,
  };
}

