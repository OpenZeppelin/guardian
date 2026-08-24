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

  /**
   * Executes the request at the given chain anchor's reference block instead
   * of the local sync height, then proves, submits, and applies. Every
   * multisig proposal execution goes through here: the signed summary binds
   * the reference block since protocol 0.16, so the collected signatures only
   * authorize an execution pinned to the proposal's anchor. There is
   * deliberately no unanchored variant — submitting a signed proposal at the
   * local sync height would reproduce the original
   * "metadata does not match tx_summary" failure.
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
