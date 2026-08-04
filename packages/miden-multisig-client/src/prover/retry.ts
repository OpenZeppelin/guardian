import type { TransactionExecution, TransactionProof } from '@miden-sdk/miden-sdk';
import type { ResolvedProverConfig } from './config.js';
import { isTransientProverError } from './errors.js';
import type { RetryRuntime } from '../retry/runtime.js';
import { productionRetryRuntime, retryDelay } from '../retry/runtime.js';

export type { RetryRuntime };
export { productionRetryRuntime, retryDelay };

export async function proveWithRetry(
  execution: TransactionExecution,
  config: ResolvedProverConfig,
  runtime: RetryRuntime = productionRetryRuntime,
): Promise<TransactionProof> {
  for (let attempt = 0; attempt < config.maxAttempts; attempt += 1) {
    try {
      const prover = config.createProver();
      return prover === undefined
        ? await execution.prove()
        : await execution.prove({ prover });
    } catch (error) {
      if (!isTransientProverError(error) || attempt + 1 >= config.maxAttempts) {
        throw error;
      }
      await runtime.sleep(retryDelay(attempt, runtime.unitRandom()));
    }
  }

  throw new Error('unreachable prover retry state');
}
