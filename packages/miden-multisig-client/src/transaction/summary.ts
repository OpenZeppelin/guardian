import type { TransactionRequest, TransactionSummary, WebClient } from '@demox-labs/miden-sdk';
import { AccountId } from '@demox-labs/miden-sdk';

export async function executeForSummary(
  client: WebClient,
  accountId: string,
  txRequest: TransactionRequest,
): Promise<TransactionSummary> {
  const acc = AccountId.fromHex(accountId);
  return (client as any).executeForSummary(acc, txRequest);
}

