import type { TransactionRequest, TransactionSummary, WebClient } from '@miden-sdk/miden-sdk';
import { AccountId } from '@miden-sdk/miden-sdk';

export async function executeForSummary(
  client: WebClient,
  accountId: string,
  txRequest: TransactionRequest,
): Promise<TransactionSummary> {
  const acc = AccountId.fromHex(accountId);
  return (client as any).executeForSummary(acc, txRequest);
}

