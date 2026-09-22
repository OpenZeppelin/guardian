import type { TransactionRequest, Word } from '@miden-sdk/miden-sdk';
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
  Word as WordType,
} from '@miden-sdk/miden-sdk';
import type { RawClientSource } from '../raw-client.js';
import { buildMultisigRequest, multisigRequestBuilder } from './authArgs.js';
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
 * P2ID storage since protocol 0.17 rc.5: target account, then a two-felt salt.
 * Zero salt is the upstream default. A secret salt hides the target from
 * guesses against the storage commitment; this builder keeps the note
 * deterministic in the proposal salt, which already derives the serial number.
 */
function p2idStorage(recipient: AccountId): Felt[] {
  return [recipient.suffix(), recipient.prefix(), new Felt(0n), new Felt(0n)];
}

/**
 * P2IDE storage: reclaimer (the sender), target, then reclaim and timelock
 * heights. Zero encodes an unset height. The script requires all six items.
 */
function p2ideStorage(
  sender: AccountId,
  recipient: AccountId,
  reclaimHeight: number,
  timelockHeight: number,
): Felt[] {
  return [
    sender.suffix(),
    sender.prefix(),
    recipient.suffix(),
    recipient.prefix(),
    new Felt(BigInt(reclaimHeight)),
    new Felt(BigInt(timelockHeight)),
  ];
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

  const noteScript = isP2ide ? NoteScript.p2ide() : NoteScript.p2id();
  const storageFelts = isP2ide
    ? p2ideStorage(sender, recipient, reclaimHeight ?? 0, timelockHeight ?? 0)
    : p2idStorage(recipient);
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
 * proposal produces on execution. Since Miden 0.16, the asset
 * callback flag is encoded in the faucet account ID.
 */
export function buildP2idNoteFromMetadata(
  senderId: string,
  recipientId: string,
  faucetId: string,
  amount: bigint,
  noteType: NoteType,
  saltHex: string,
  heights: P2ideHeightOptions = {},
): Note {
  const sender = AccountId.fromHex(senderId);
  const recipient = AccountId.fromHex(recipientId);
  const faucet = AccountId.fromHex(faucetId);

  const asset = new FungibleAsset(faucet, amount);
  const noteAssets = new NoteAssets([asset]);

  return buildP2idNote(sender, recipient, noteAssets, noteType, saltHex, heights);
}

export async function buildP2idTransactionRequest(
  client: RawClientSource,
  senderId: string,
  recipientId: string,
  faucetId: string,
  amount: bigint,
  options: P2idTransactionOptions = {},
): Promise<{ request: TransactionRequest; salt: Word }> {
  const { builder, saltHex } = await multisigRequestBuilder(client, {
    ...options,
    accountId: senderId,
  });

  const note = buildP2idNoteFromMetadata(
    senderId,
    recipientId,
    faucetId,
    amount,
    options.noteType ?? NoteType.Public,
    saltHex,
    { reclaimHeight: options.reclaimHeight, timelockHeight: options.timelockHeight },
  );

  let txBuilder = builder.withOwnOutputNotes(new MidenArrays.NoteArray([note]));

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
  }

  return buildMultisigRequest(txBuilder, saltHex, senderId);
}
