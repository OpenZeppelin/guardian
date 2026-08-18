import type {
  MidenClient,
  TransactionRequest,
  TransactionSummary,
  WasmWebClient,
} from '@miden-sdk/miden-sdk';
import { AccountId, Word } from '@miden-sdk/miden-sdk';
import { getRawMidenClient } from '../raw-client.js';

/**
 * Index of the first user param carrying the auth-arg salt. The guarded-multisig
 * auth component zeroes user params 0-2 and fills 3-6 with the auth args, matching
 * `push.0.0.0` ahead of `multisig::auth_tx` in `guarded_multisig.masm`.
 */
const SALT_USER_PARAM_OFFSET = 3;

export function executeForSummary(
  client: MidenClient,
  accountId: string,
  txRequest: TransactionRequest,
  midenRpcEndpoint: string,
): Promise<TransactionSummary>;
export function executeForSummary(
  client: WasmWebClient,
  accountId: string,
  txRequest: TransactionRequest,
  midenRpcEndpoint?: string,
): Promise<TransactionSummary>;
export async function executeForSummary(
  client: MidenClient | WasmWebClient,
  accountId: string,
  txRequest: TransactionRequest,
  midenRpcEndpoint?: string,
): Promise<TransactionSummary> {
  const acc = AccountId.fromHex(accountId);
  const rawClient = await getRawMidenClient(client, midenRpcEndpoint);
  return rawClient.executeForSummary(acc, txRequest);
}

/**
 * Reads the auth-arg salt back out of a transaction summary.
 *
 * Since miden-protocol 0.16-rc the summary binds seven user-defined elements
 * instead of a dedicated salt word. The guarded-multisig auth component zeroes
 * the leading three and passes the auth args as the trailing four, so the salt
 * is the tail of `userParams()`.
 */
export function summarySalt(summary: TransactionSummary): Word {
  return Word.newFromFelts(summary.userParams().slice(SALT_USER_PARAM_OFFSET));
}
