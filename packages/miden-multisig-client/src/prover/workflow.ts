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

  async submit(accountId: AccountId, request: TransactionRequest): Promise<void> {
    const execution = await this.client.transactions.executeRequest(accountId, request);
    const proof = await proveWithRetry(execution, this.config, this.runtime);
    const submission = await proof.submit();
    await submission.apply();
  }

  /**
   * Executes the request at the given chain anchor's reference block instead
   * of the local sync height, then proves, submits, and applies — the anchored
   * counterpart of {@link submit} for executing signed multisig proposals,
   * whose summary only reproduces at the block it was proposed at.
   */
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
