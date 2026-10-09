import type { MidenClient, TransactionRequest, TransactionSummary } from '@miden-sdk/miden-sdk';
import { Word } from '@miden-sdk/miden-sdk';
import {
  BoundBlockNotDeclaredError,
  TransactionSummaryLayoutError,
} from '../multisig/authArgErrors.js';
import { requestBoundBlockNum } from './authArgs.js';

/**
 * Layout of the six user params a multisig auth component binds into the
 * transaction summary since protocol 0.17: the approval expiration block (or
 * zero for an approval that never expires), a zero, then the four salt felts.
 */
const APPROVAL_EXPIRATION_USER_PARAM_INDEX = 0;
const SALT_USER_PARAM_OFFSET = 2;

/**
 * The serialized summary layout {@link summaryBoundBlockNum} reads, from
 * `TransactionSummary::write_into` at miden-protocol 0.17.0: the version byte
 * first, then variable-length fields, then a fixed tail of the block number
 * (u32, little endian), the block commitment (four 8-byte felts), the
 * expiration delta (u16) and the six user-param felts, with no length prefix.
 */
const SUPPORTED_SUMMARY_VERSION = 1;
const BLOCK_NUMBER_BYTES = 4;
const BLOCK_COMMITMENT_BYTES = 32;
const SUMMARY_TAIL_BYTES = BLOCK_NUMBER_BYTES + BLOCK_COMMITMENT_BYTES + 2 + 6 * 8;

/**
 * The Miden client synced and its node still has not produced the block a
 * proposal binds, so the proposal cannot execute at this client's tip yet.
 * Worth retrying once the node catches up.
 */
export class ChainBehindBoundBlockError extends Error {
  readonly retryable = true;
  readonly syncHeight: number;
  readonly boundBlockNum: number;

  constructor(details: { syncHeight: number; boundBlockNum: number }) {
    super(
      `the Miden client synced to block ${details.syncHeight}, below block ` +
        `${details.boundBlockNum} the proposal binds; its node has not reached that block yet`,
    );
    this.name = 'ChainBehindBoundBlockError';
    this.syncHeight = details.syncHeight;
    this.boundBlockNum = details.boundBlockNum;
  }
}

/**
 * Whether a failed re-execution came from chain state this client can catch up
 * with rather than from the proposal itself: a node that has not reached the
 * bound block yet, or account state the node pruned because this client had
 * not synced recently. Either clears on a later attempt, which syncs first.
 */
export function isStaleChainError(error: unknown): boolean {
  if (error instanceof ChainBehindBoundBlockError) {
    return true;
  }
  const message = error instanceof Error ? error.message : String(error);
  return message.includes('has been pruned');
}

/**
 * Executes a multisig request at the chain tip to obtain the summary awaiting
 * authorization. Proposers derive a new proposal's summary with it, and
 * cosigners and the executor reproduce it, whatever block they have synced to.
 *
 * Since protocol 0.17 a multisig summary binds the block its auth args name
 * (the bound block), not the block the transaction executes against, so it
 * reproduces at any later tip once the bound block is in the transaction's
 * partial blockchain. The request declares it through `withBlockNumbers`, and
 * foreign accounts, the fee faucet among them, load at the tip. Executing at
 * the bound block instead would load them there, which a node prunes about 50
 * blocks later (issue #462).
 *
 * The client has to have synced to at least the bound block. When it has not,
 * this syncs once before executing.
 *
 * @throws BoundBlockNotDeclaredError when the request binds a block in its
 *   multisig auth args without declaring it.
 */
export async function executeForSummaryAtTip(
  client: MidenClient,
  accountId: string,
  txRequest: TransactionRequest,
): Promise<TransactionSummary> {
  await prepareTipExecution(client, txRequest);
  return client.transactions.preview({
    operation: 'custom',
    account: accountId,
    request: txRequest,
  });
}

/**
 * Gets `client` ready to execute `request` at the chain tip: checks the
 * request declares the block its multisig auth args bind, and syncs to that
 * block (see {@link syncToBoundBlock}).
 *
 * @throws BoundBlockNotDeclaredError when the request binds a block in its
 *   multisig auth args without declaring it.
 */
export async function prepareTipExecution(
  client: MidenClient,
  request: TransactionRequest,
  syncChain?: () => Promise<unknown>,
): Promise<void> {
  const boundBlockNum = requireDeclaredBoundBlock(request);
  if (boundBlockNum !== undefined) {
    await syncToBoundBlock(client, boundBlockNum, syncChain);
  }
}

/**
 * The block `request`'s multisig auth args bind, after checking the request
 * declares it. `undefined` for a request without multisig auth args, which
 * has no bound block to declare.
 *
 * @throws BoundBlockNotDeclaredError when the block is bound but not declared.
 */
export function requireDeclaredBoundBlock(request: TransactionRequest): number | undefined {
  const boundBlockNum = requestBoundBlockNum(request);
  if (boundBlockNum !== undefined && !request.blockNumbers().includes(boundBlockNum)) {
    throw new BoundBlockNotDeclaredError(boundBlockNum);
  }
  return boundBlockNum;
}

/**
 * Syncs `client` once when its sync height is below `blockNum`, the block a
 * proposal binds. Execution at a tip below it fails with "requested block N is
 * after transaction reference block M", and a store that has never synced (a
 * cosigner that has only just loaded the account) holds no header to rebuild
 * the request from. The sync is a chain sync; `syncChain` lets a caller wrap
 * it in its own retry policy.
 *
 * This does not make a store that is already past the bound block current. An
 * execution loads foreign accounts, the fee faucet among them, at the store's
 * sync height, which a node prunes about 50 blocks later, so the multisig
 * entry points that re-execute a proposal sync the chain first.
 *
 * @throws ChainBehindBoundBlockError when the node has not reached the block.
 */
export async function syncToBoundBlock(
  client: MidenClient,
  blockNum: number,
  syncChain: () => Promise<unknown> = () => client.syncChain(),
): Promise<void> {
  if ((await client.getSyncHeight()) >= blockNum) {
    return;
  }
  await syncChain();
  const syncHeight = await client.getSyncHeight();
  if (syncHeight < blockNum) {
    throw new ChainBehindBoundBlockError({ syncHeight, boundBlockNum: blockNum });
  }
}

/**
 * Reads the block a multisig transaction summary binds, the counterpart of the
 * Rust `TransactionSummary::block_number`. Since protocol 0.17 that is the block
 * the request's multisig auth args name, so it is the proposal's bound block.
 *
 * The web SDK exposes no accessor for it, so this reads the serialized summary
 * (layout above) and checks the block commitment next to it against
 * `blockCommitment()`, so a layout this client does not know is refused rather
 * than read at the wrong offset.
 *
 * @throws TransactionSummaryLayoutError when the summary is not version 1, is
 *   too short to hold the tail, or its tail does not hold its block commitment.
 */
export function summaryBoundBlockNum(summary: TransactionSummary): number {
  const bytes = summary.serialize();
  const blockCommitment = summary.blockCommitment();
  try {
    return readBoundBlockNum(bytes, blockCommitment.serialize());
  } finally {
    blockCommitment.free();
  }
}

function readBoundBlockNum(bytes: Uint8Array, blockCommitment: Uint8Array): number {
  if (bytes.length < 1 + SUMMARY_TAIL_BYTES) {
    throw new TransactionSummaryLayoutError(
      `${bytes.length} bytes is shorter than the version byte and the ${SUMMARY_TAIL_BYTES}-byte tail`,
    );
  }
  if (bytes[0] !== SUPPORTED_SUMMARY_VERSION) {
    throw new TransactionSummaryLayoutError(
      `version ${bytes[0]}, but only version ${SUPPORTED_SUMMARY_VERSION} is supported`,
    );
  }
  const blockNumberOffset = bytes.length - SUMMARY_TAIL_BYTES;
  const commitmentOffset = blockNumberOffset + BLOCK_NUMBER_BYTES;
  const tailCommitment = bytes.subarray(commitmentOffset, commitmentOffset + BLOCK_COMMITMENT_BYTES);
  if (!bytesEqual(tailCommitment, blockCommitment)) {
    throw new TransactionSummaryLayoutError(
      'the bytes after the block number are not the summary block commitment',
    );
  }
  return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(
    blockNumberOffset,
    true,
  );
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((byte, index) => byte === b[index]);
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
