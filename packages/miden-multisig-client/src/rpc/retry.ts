import type { ResolvedRpcConfig } from './config.js';
import { isTransientRpcError } from './errors.js';
import type { RetryRuntime } from '../retry/runtime.js';
import { productionRetryRuntime, retryDelay } from '../retry/runtime.js';

export async function retryRpcRead<T>(
  operation: () => Promise<T>,
  config: ResolvedRpcConfig,
  runtime: RetryRuntime = productionRetryRuntime,
): Promise<T> {
  for (let attempt = 0; attempt < config.maxAttempts; attempt += 1) {
    try {
      return await operation();
    } catch (error) {
      if (!isTransientRpcError(error) || attempt + 1 >= config.maxAttempts) {
        throw error;
      }
      await runtime.sleep(retryDelay(attempt, runtime.unitRandom()));
    }
  }

  throw new Error('unreachable rpc retry state');
}
