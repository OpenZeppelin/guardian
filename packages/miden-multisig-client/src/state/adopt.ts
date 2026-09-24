/**
 * Deciding when GUARDIAN-provided account state may replace the local store.
 *
 * Both entry points that pull an account from GUARDIAN need this: `syncState`,
 * which refreshes an account already in hand, and `MultisigClient.load`, which
 * rebuilds one. Sharing the rule is the point. Deciding it twice is how the two
 * paths came to disagree, with `load` keeping whatever the store held whenever
 * it held anything, so a caller that had touched the account before read stale
 * state with no error.
 */

import { Account, AccountId, Endpoint, RpcClient } from '@miden-sdk/miden-sdk';

import { retryRpcRead } from '../rpc/retry.js';
import type { ResolvedRpcConfig } from '../rpc/config.js';
import { normalizeHexWord } from '../utils/encoding.js';

const ZERO_COMMITMENT = `0x${'0'.repeat(64)}`;

/**
 * The account's commitment as the node reports it, or `null` when the account
 * is not deployed yet. An undeployed account is not an error: it has no
 * on-chain commitment to disagree with.
 */
export async function readOnChainCommitment(
  midenRpcEndpoint: string,
  accountId: AccountId,
  rpcConfig: ResolvedRpcConfig,
): Promise<string | null> {
  const rpcClient = new RpcClient(new Endpoint(midenRpcEndpoint));

  try {
    const accountDetails = await retryRpcRead(
      () => rpcClient.getAccountDetails(accountId),
      rpcConfig,
    );
    if (!accountDetails) {
      return null;
    }
    const commitment = normalizeHexWord(accountDetails.commitment().toHex());
    return commitment === ZERO_COMMITMENT ? null : commitment;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (
      message.includes('null pointer passed to rust') ||
      message.includes('No account header record found for given ID') ||
      message.toLowerCase().includes('not found')
    ) {
      return null;
    }
    throw error;
  }
}

/**
 * Decide whether GUARDIAN-provided state may overwrite the local store.
 *
 * Returns `false`, rather than throwing, when the GUARDIAN state is simply
 * *behind* local (lower nonce). That happens whenever the execution delta the
 * client pushed has not been canonicalized by GUARDIAN's background worker yet
 * (see OpenZeppelin/guardian#316), or permanently if that candidate was
 * discarded (#312 / #319). In that case the local account is already ahead and
 * is independently verifiable against chain (`verifyStateCommitment`), so it is
 * authoritative and must be kept, not clobbered; the caller keeps local and
 * refreshes config from it.
 *
 * Still throws for genuine divergence: an incoming state at the *same* nonce as
 * local but a different commitment, or an incoming state whose commitment does
 * not match the on-chain commitment.
 *
 * Callers must only reach here once they know the two states differ. At equal
 * nonce this treats the pair as divergent, which is true only when the
 * commitments already disagree.
 */
export async function isSafeToAdoptGuardianState(params: {
  accountId: string;
  incomingAccount: Account;
  localAccount?: Account;
  readCommitment: () => Promise<string | null>;
}): Promise<boolean> {
  const { accountId, incomingAccount, localAccount, readCommitment } = params;

  if (localAccount) {
    const localNonce = localAccount.nonce().asInt();
    const incomingNonce = incomingAccount.nonce().asInt();

    if (incomingNonce < localNonce) {
      return false;
    }

    if (incomingNonce === localNonce) {
      throw new Error(
        `Refusing to overwrite local state: incoming nonce ${incomingNonce.toString()} equals local nonce ${localNonce.toString()} but commitments differ for account ${accountId}`
      );
    }
  }

  const onChainCommitment = await readCommitment();
  if (!onChainCommitment) {
    // No on-chain commitment means the account is not deployed, and an
    // undeployed account has nothing to disagree with. That reading is only
    // safe when the account has never transacted: `readOnChainCommitment`
    // reports a missing account by matching `not found` in the error text, and
    // a proxy or gateway 404 says exactly that. An account with a non-zero
    // nonce has transacted, so it is deployed, so this is an RPC failure rather
    // than an undeployed account, and adopting on it would skip the commitment
    // check entirely.
    // Keyed on the *local* nonce only. An account this client has transacted is
    // deployed, so a node reporting nothing for it is an RPC failure rather
    // than an undeployed account, and adopting there would skip the check
    // against chain. The incoming nonce is GUARDIAN's claim rather than
    // evidence, and `syncState` legitimately meets a higher incoming nonce with
    // no local history; the caller that has no local record at all applies its
    // own rule.
    const transacted = localAccount?.nonce().asInt() ?? BigInt(0);
    if (transacted > BigInt(0)) {
      throw new Error(
        `Refusing to overwrite local state: account ${accountId} has transacted (nonce ${transacted.toString()}) but the node reported no on-chain commitment, so the incoming state could not be checked against chain`
      );
    }
    return true;
  }

  const incomingCommitment = normalizeHexWord(incomingAccount.to_commitment().toHex());
  if (incomingCommitment !== onChainCommitment) {
    throw new Error(
      `Refusing to overwrite local state: incoming commitment does not match on-chain commitment for account ${accountId}`
    );
  }

  return true;
}
