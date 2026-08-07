import type { TransactionExecution, TransactionProof } from '@miden-sdk/miden-sdk';
import type { ResolvedProverConfig } from './config.js';
import { isTransientProverError } from './errors.js';
import type { RetryRuntime } from '../retry/runtime.js';
import { productionRetryRuntime, retryTransient } from '../retry/runtime.js';

export async function proveWithRetry(
  execution: TransactionExecution,
  config: ResolvedProverConfig,
  runtime: RetryRuntime = productionRetryRuntime,
): Promise<TransactionProof> {
  return retryTransient(
    async () => {
      const prover = config.createProver();
      return prover === undefined ? await execution.prove() : await execution.prove({ prover });
    },
    config.maxAttempts,
    isTransientProverError,
    runtime,
  );
}
