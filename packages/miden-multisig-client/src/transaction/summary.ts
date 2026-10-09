import type { MidenClient, TransactionRequest, TransactionSummary } from '@miden-sdk/miden-sdk';
import { ChainAnchor, Word } from '@miden-sdk/miden-sdk';
import { BoundBlockNotDeclaredError } from '../multisig/authArgErrors.js';
import { base64ToUint8Array } from '../utils/encoding.js';
import { requestBoundBlockNum } from './authArgs.js';

/**
 * Layout of the six user params a multisig auth component binds into the
 * transaction summary since protocol 0.17: the approval expiration block (or
 * zero for an approval that never expires), a zero, then the four salt felts.
 */
const APPROVAL_EXPIRATION_USER_PARAM_INDEX = 0;
const SALT_USER_PARAM_OFFSET = 2;

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
 * The block a legacy `chainAnchor` names, for a proposal a 0.18 client made
 * before `boundBlockNum` existed. Decodes the anchor for the one number and
 * frees it.
 */
export function legacyChainAnchorBlockNum(anchorBase64: string): number {
  const anchor = ChainAnchor.deserialize(base64ToUint8Array(anchorBase64));
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
