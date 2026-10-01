import type { MidenClient, TransactionRequest, TransactionSummary } from '@miden-sdk/miden-sdk';
import { ChainAnchor, Word } from '@miden-sdk/miden-sdk';
import { BoundBlockNotDeclaredError } from '../multisig/authArgErrors.js';
import { base64ToUint8Array, normalizeHexWord, uint8ArrayToBase64 } from '../utils/encoding.js';
import { requestBoundBlockNum } from './authArgs.js';

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
 * Derives the summary awaiting authorization for a proposal the caller is
 * creating now, and captures a `ChainAnchor` at the current sync height to ship
 * with it.
 *
 * The summary is derived at the chain tip, like every other execution of a
 * multisig proposal (see {@link executeForSummaryAtTip}). The anchor still
 * travels in the proposal: it names the block the request's auth args bind,
 * which is how a rebuild learns that block, and 0.18.0-rc.1 clients re-execute
 * at it. A proposer builds at the sync height the anchor is captured at, and
 * the check below is what makes that hold.
 */
export async function executeForSummary(
  client: MidenClient,
  accountId: string,
  txRequest: TransactionRequest,
): Promise<{ summary: TransactionSummary; anchor: ChainAnchor }> {
  const anchor = await client.transactions.captureAnchor(txRequest);
  let summary: TransactionSummary;
  try {
    summary = await executeForSummaryAtTip(client, accountId, txRequest);
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
 * Executes a multisig request at the chain tip to obtain the summary awaiting
 * authorization. This is how cosigners and the executor reproduce a proposal's
 * summary, whatever block they have synced to.
 *
 * Since protocol 0.17 a multisig summary binds the block its auth args name
 * (the bound block), not the block the transaction executes against, so it
 * reproduces at any later tip once the bound block is in the transaction's
 * partial blockchain. The request declares it through `withBlockNumbers`, and
 * foreign accounts, the fee faucet among them, load at the tip. Re-executing at
 * the proposal's anchor instead loads them at the bound block, which a node
 * prunes about 50 blocks later (issue #462).
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
 * Executes a transaction at the given `ChainAnchor`'s reference block to
 * obtain the summary awaiting authorization.
 *
 * For a summary that binds the reference block, such as a single-signature
 * one. A multisig proposal's summary binds its bound block instead and is
 * reproduced with {@link executeForSummaryAtTip}: re-executing it at an anchor
 * fails once the node prunes the anchor block's account state.
 */
export async function executeForSummaryAt(
  client: MidenClient,
  accountId: string,
  txRequest: TransactionRequest,
  anchor: ChainAnchor,
): Promise<TransactionSummary> {
  return client.transactions.preview({
    operation: 'custom',
    account: accountId,
    request: txRequest,
    anchor,
  });
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
 * transaction summary before the block it names is taken as the one the
 * summary binds.
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
