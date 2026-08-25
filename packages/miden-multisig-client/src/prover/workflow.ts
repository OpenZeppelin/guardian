import type {
  AccountId,
  ChainAnchor,
  MidenClient,
  TransactionRequest,
} from '@miden-sdk/miden-sdk';
import type { ResolvedProverConfig } from './config.js';
import type { RetryRuntime } from '../retry/runtime.js';
import { proveWithRetry } from './retry.js';

export class ProverWorkflow {
  constructor(
    private readonly client: MidenClient,
    private readonly config: ResolvedProverConfig,
    private readonly runtime?: RetryRuntime,
  ) {}

  /** Proves, submits, and applies a request at its signed chain anchor. */
  async submitAt(
    accountId: AccountId,
    request: TransactionRequest,
    anchor: ChainAnchor,
  ): Promise<void> {
    const execution = await this.client.transactions.executeRequest(accountId, request, {
      anchor,
    });
    const proof = await proveWithRetry(execution, this.config, this.runtime);
    const submission = await proof.submit();
    await submission.apply();
  }
}
