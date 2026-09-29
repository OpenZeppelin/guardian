import type {
  AccountId,
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

  /**
   * Executes a request at the chain tip, then proves, submits, and applies it.
   * A multisig proposal's request declares the block its summary binds, so the
   * signed summary reproduces at the tip.
   */
  async submit(accountId: AccountId, request: TransactionRequest): Promise<void> {
    const execution = await this.client.transactions.executeRequest(accountId, request);
    const proof = await proveWithRetry(execution, this.config, this.runtime);
    const submission = await proof.submit();
    await submission.apply();
  }
}
