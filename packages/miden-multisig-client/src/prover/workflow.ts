import type { AccountId, MidenClient, TransactionRequest } from '@miden-sdk/miden-sdk';
import type { ResolvedProverConfig } from './config.js';
import type { RetryRuntime } from './retry.js';
import { proveWithRetry } from './retry.js';

export class ProverWorkflow {
  constructor(
    private readonly client: MidenClient,
    private readonly config: ResolvedProverConfig,
    private readonly runtime?: RetryRuntime,
  ) {}

  async submit(accountId: AccountId, request: TransactionRequest): Promise<void> {
    const execution = await this.client.transactions.executeRequest(accountId, request);
    const proof = await proveWithRetry(execution, this.config, this.runtime);
    const submission = await proof.submit();
    await submission.apply();
  }
}
