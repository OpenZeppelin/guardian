import type {
  MidenClient,
  TransactionRequest,
  TransactionSummary,
  WasmWebClient,
} from '@miden-sdk/miden-sdk';
import { AccountId } from '@miden-sdk/miden-sdk';
import { getRawMidenClient } from '../raw-client.js';

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
