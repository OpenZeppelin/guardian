import type { ResolvedRpcConfig } from './config.js';
import { isTransientRpcError } from './errors.js';
import type { RetryRuntime } from '../retry/runtime.js';
import { productionRetryRuntime, retryTransient } from '../retry/runtime.js';

export async function retryRpcRead<T>(
  operation: () => Promise<T>,
  config: ResolvedRpcConfig,
  runtime: RetryRuntime = productionRetryRuntime,
): Promise<T> {
  return retryTransient(operation, config.maxAttempts, isTransientRpcError, runtime);
}
