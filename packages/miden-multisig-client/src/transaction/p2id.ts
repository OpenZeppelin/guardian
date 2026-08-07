import type { Account, TransactionRequest, Word } from '@miden-sdk/miden-sdk';
import {
  AccountId,
  Felt,
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
import { parseP2ideHeight } from '../types/proposal.js';

/**
 * P2IDE execution constraints (issue #366). Presence of either height builds
 * a P2IDE note instead of a plain P2ID note, mirroring the Miden SDK's
 * `reclaimAfter`/`timelockUntil` semantics on `SendOptions`.
 */
export interface P2ideHeightOptions {
  /** Absolute block height at which the sender may reclaim the note. */
  reclaimHeight?: number;
  /** Absolute block height before which the note cannot be consumed. */
  timelockHeight?: number;
}

export interface P2idTransactionOptions extends SignatureOptions, P2ideHeightOptions {
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

/**
 * Builds the fungible asset to transfer, sourcing the callback flag from the held asset.
 *
 * In Miden 0.15 the callback flag is part of the vault key, so rebuilding the asset from
 * `faucet`/`amount` with the default flag would not match the held asset and the transfer
 * would abort. When the faucet is absent the default flag is used, surfacing the
 * missing-asset error during execution.
 */
function resolveFungibleAssetFromVault(
  account: Account,
  faucet: AccountId,
  amount: bigint,
): FungibleAsset {
  const faucetHex = faucet.toString();
  const assetFromVault = account
    .vault()
    .fungibleAssets()
    .find(asset => asset.faucetId().toString() === faucetHex);

  if (!assetFromVault) {
    return new FungibleAsset(faucet, amount);
  }

  return FungibleAsset.fromVaultKey(assetFromVault.vaultKey(), amount);
}

function buildP2idNote(
  sender: AccountId,
  recipient: AccountId,
  noteAssets: NoteAssets,
  noteType: NoteType,
  saltHex: string,
  heights: P2ideHeightOptions = {},
): Note {
  const salt = WordType.fromHex(normalizeHexWord(saltHex));
  const serialNum = deriveP2idSerialNumber(salt);

  const reclaimHeight = parseP2ideHeight('reclaimHeight', heights.reclaimHeight);
  const timelockHeight = parseP2ideHeight('timelockHeight', heights.timelockHeight);
  const isP2ide = reclaimHeight !== undefined || timelockHeight !== undefined;

  // P2IDE storage layout (miden-standards `P2ideNoteStorage`): the P2ID
  // storage plus reclaim/timelock heights as felts, 0 encoding "unset".
  const noteScript = isP2ide ? NoteScript.p2ide() : NoteScript.p2id();
  const storageFelts = [
    recipient.suffix(),
    recipient.prefix(),
  ];
  if (isP2ide) {
    storageFelts.push(
      new Felt(BigInt(reclaimHeight ?? 0)),
      new Felt(BigInt(timelockHeight ?? 0)),
    );
  }
  const noteStorage = new NoteStorage(new FeltArray(storageFelts));

  const noteRecipient = new NoteRecipient(serialNum, noteScript, noteStorage);
  const noteTag = NoteTag.withAccountTarget(recipient);

  const noteMetadata = new NoteMetadata(
    sender,
    noteType,
    noteTag,
  );

  return new Note(noteAssets, noteMetadata, noteRecipient);
}

/**
 * Rebuilds the P2ID note a proposal creates, from its metadata fields. The
 * note is deterministic in the salt, so the resulting ID matches the note the
 * proposal produces on execution (issue #356). The asset is derived from the
 * account's current vault state, so call this before the transfer executes.
 */
export function buildP2idNoteFromMetadata(
  senderId: string,
  recipientId: string,
  faucetId: string,
  amount: bigint,
  account: Account,
  noteType: NoteType,
  saltHex: string,
  heights: P2ideHeightOptions = {},
): Note {
  const sender = AccountId.fromHex(senderId);
  const recipient = AccountId.fromHex(recipientId);
  const faucet = AccountId.fromHex(faucetId);

  const asset = resolveFungibleAssetFromVault(account, faucet, amount);
  const noteAssets = new NoteAssets([asset]);

  return buildP2idNote(sender, recipient, noteAssets, noteType, saltHex, heights);
}

export function buildP2idTransactionRequest(
  senderId: string,
  recipientId: string,
  faucetId: string,
  amount: bigint,
  account: Account,
  options: P2idTransactionOptions = {},
): { request: TransactionRequest; salt: Word } {
  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();

  const note = buildP2idNoteFromMetadata(
    senderId,
    recipientId,
    faucetId,
    amount,
    account,
    options.noteType ?? NoteType.Public,
    authSaltHex,
    { reclaimHeight: options.reclaimHeight, timelockHeight: options.timelockHeight },
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
