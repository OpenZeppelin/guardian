import type {
  MidenClient,
  TransactionRequest,
  TransactionSummary,
  WasmWebClient,
} from '@miden-sdk/miden-sdk';
import { AccountId, ChainAnchor, Word } from '@miden-sdk/miden-sdk';
import { getRawMidenClient } from '../raw-client.js';
import { base64ToUint8Array, normalizeHexWord, uint8ArrayToBase64 } from '../utils/encoding.js';

/**
 * Layout of the six user params a multisig auth component binds into the
 * transaction summary since protocol 0.17: the approval expiration block (or
 * zero for an approval that never expires), a zero, then the four salt felts.
 */
const APPROVAL_EXPIRATION_USER_PARAM_INDEX = 0;
const SALT_USER_PARAM_OFFSET = 2;

/**
 * The summary binds the block the request's auth args name, and the anchor
 * the store's sync height at capture. A sync landing between the build and the
 * capture leaves them one block apart, and every cosigner's anchor check would
 * then fail on a proposal nothing else is wrong with. Caught here, before the
 * proposal is pushed, so the proposer rebuilds instead.
 */
export class SummaryAnchorMismatchError extends Error {
  readonly retryable = true;

  constructor(details: { anchorCommitmentHex: string; summaryBlockCommitmentHex: string }) {
    super(
      `the transaction summary binds block commitment ${details.summaryBlockCommitmentHex} but ` +
        `the captured chain anchor is ${details.anchorCommitmentHex}; a sync landed between ` +
        'building the request and capturing its anchor, so rebuild the request and retry',
    );
    this.name = 'SummaryAnchorMismatchError';
  }
}

/**
 * Captures a `ChainAnchor` for the request at the current sync height and
 * executes the transaction against it to obtain the summary awaiting
 * authorization. The anchor is returned alongside the summary so the proposer
 * can ship it with the signed data; cosigners and the executor then reproduce
 * the summary with {@link executeForSummaryAt} regardless of their own sync
 * height.
 *
 * The request's auth args bind the block its summary commits to, and this
 * package pins that block to the anchor: a proposer builds at the sync height
 * the anchor is captured at, and a rebuild passes the anchor's block number.
 * The check below is what makes the first half hold.
 */
export function executeForSummary(
  client: MidenClient,
  accountId: string,
  txRequest: TransactionRequest,
  midenRpcEndpoint: string,
): Promise<{ summary: TransactionSummary; anchor: ChainAnchor }>;
export function executeForSummary(
  client: WasmWebClient,
  accountId: string,
  txRequest: TransactionRequest,
  midenRpcEndpoint?: string,
): Promise<{ summary: TransactionSummary; anchor: ChainAnchor }>;
export async function executeForSummary(
  client: MidenClient | WasmWebClient,
  accountId: string,
  txRequest: TransactionRequest,
  midenRpcEndpoint?: string,
): Promise<{ summary: TransactionSummary; anchor: ChainAnchor }> {
  const acc = AccountId.fromHex(accountId);
  const rawClient = await getRawMidenClient(client, midenRpcEndpoint);
  const anchor = await rawClient.chainAnchorForRequest(txRequest);
  let summary: TransactionSummary;
  try {
    summary = await rawClient.executeForSummaryAt(acc, txRequest, anchor);
  } catch (error) {
    anchor.free();
    throw error;
  }

  const anchorCommitment = anchor.commitment();
  const summaryBlockCommitment = summary.blockCommitment();
  const anchorCommitmentHex = normalizeHexWord(anchorCommitment.toHex());
  const summaryBlockCommitmentHex = normalizeHexWord(summaryBlockCommitment.toHex());
  anchorCommitment.free?.();
  summaryBlockCommitment.free?.();
  if (anchorCommitmentHex !== summaryBlockCommitmentHex) {
    anchor.free();
    throw new SummaryAnchorMismatchError({ anchorCommitmentHex, summaryBlockCommitmentHex });
  }
  return { summary, anchor };
}

/**
 * Executes a transaction at the given `ChainAnchor`'s reference block to
 * obtain the summary awaiting authorization: the anchored counterpart of
 * {@link executeForSummary} for cosigners and executors holding a proposal's
 * anchor.
 */
export function executeForSummaryAt(
  client: MidenClient,
  accountId: string,
  txRequest: TransactionRequest,
  anchor: ChainAnchor,
  midenRpcEndpoint: string,
): Promise<TransactionSummary>;
export function executeForSummaryAt(
  client: WasmWebClient,
  accountId: string,
  txRequest: TransactionRequest,
  anchor: ChainAnchor,
  midenRpcEndpoint?: string,
): Promise<TransactionSummary>;
export async function executeForSummaryAt(
  client: MidenClient | WasmWebClient,
  accountId: string,
  txRequest: TransactionRequest,
  anchor: ChainAnchor,
  midenRpcEndpoint?: string,
): Promise<TransactionSummary> {
  const acc = AccountId.fromHex(accountId);
  const rawClient = await getRawMidenClient(client, midenRpcEndpoint);
  return rawClient.executeForSummaryAt(acc, txRequest, anchor);
}

/**
 * Serializes a `ChainAnchor` to base64 for the proposal wire payload.
 */
export function chainAnchorToBase64(anchor: ChainAnchor): string {
  return uint8ArrayToBase64(anchor.serialize());
}

/**
 * Deserializes a `ChainAnchor` from its base64 wire form. `ChainAnchor`
 * deserialization validates the header/chain consistency internally, so a
 * decoded anchor only needs its block commitment checked against the signed
 * transaction summary before it is safe to execute against.
 */
export function chainAnchorFromBase64(anchorBase64: string): ChainAnchor {
  return ChainAnchor.deserialize(base64ToUint8Array(anchorBase64));
}

/**
 * The block a proposal's `chainAnchor` names, which is the block its summary
 * binds: a custom producer rebuilds its request at this block. Decodes the
 * anchor for the one number and frees it.
 */
export function chainAnchorBlockNum(anchorBase64: string): number {
  const anchor = chainAnchorFromBase64(anchorBase64);
  try {
    return anchor.blockNum();
  } finally {
    anchor.free();
  }
}

/**
 * Reads the salt a multisig transaction summary binds.
 *
 * Since protocol 0.17 the multisig auth components bind the salt itself into
 * the summary's user params rather than a commitment derived from it, so the
 * value cosigners signed over is readable again. A proposal still carries the
 * salt in its metadata, because a request has to be rebuilt before any summary
 * exists; this reader is the cross-check that the two agree.
 */
export function summarySalt(summary: TransactionSummary): Word {
  return Word.newFromFelts(
    summary.userParams().slice(SALT_USER_PARAM_OFFSET, SALT_USER_PARAM_OFFSET + 4),
  );
}

/**
 * Reads the block at which the approvers' signatures stop authorizing the
 * transaction, or `undefined` for an approval that never expires, which is
 * what this package's builders produce.
 */
export function summaryApprovalExpirationBlockNum(summary: TransactionSummary): number | undefined {
  const value = summary.userParams()[APPROVAL_EXPIRATION_USER_PARAM_INDEX].asInt();
  return value === 0n ? undefined : Number(value);
}
