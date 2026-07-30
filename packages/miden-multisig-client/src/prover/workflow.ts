import type { AccountId, TransactionRequest, WasmWebClient } from '@miden-sdk/miden-sdk';
import type { ResolvedProverConfig } from './config.js';
import type { RetryRuntime } from './retry.js';
import { proveWithRetry } from './retry.js';

export class ProverWorkflow {
  constructor(
    private readonly rawClient: Promise<WasmWebClient>,
    private readonly config: ResolvedProverConfig,
    private readonly runtime?: RetryRuntime,
  ) {}

  async submit(accountId: AccountId, request: TransactionRequest): Promise<void> {
    const client = await this.rawClient;
    const result = await client.executeTransaction(accountId, request);
    const proof = await proveWithRetry(client, result, this.config, this.runtime);
    const submissionHeight = await client.submitProvenTransaction(proof, result);
    await client.applyTransaction(result, submissionHeight);
  }
}
