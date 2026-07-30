import type {
  ProvenTransaction,
  TransactionResult,
  WasmWebClient,
} from '@miden-sdk/miden-sdk';
import type { ResolvedProverConfig } from './config.js';
import { isTransientProverError } from './errors.js';

const BASE_DELAY_MS = 500;
const MAX_DELAY_MS = 8_000;

export interface RetryRuntime {
  sleep(delayMs: number): Promise<void>;
  unitRandom(): number;
}

export const productionRetryRuntime: RetryRuntime = {
  sleep: (delayMs) => new Promise((resolve) => setTimeout(resolve, delayMs)),
  unitRandom: () => Math.random(),
};

export function retryDelay(retryIndex: number, unitRandom: number): number {
  const exponent = Math.min(retryIndex, 1023);
  const raw = BASE_DELAY_MS * 2 ** exponent;
  const boundedRandom = Math.min(Math.max(unitRandom, 0), 1 - Number.EPSILON);
  return Math.min(Math.floor(raw * (0.75 + boundedRandom * 0.5)), MAX_DELAY_MS);
}

export async function proveWithRetry(
  client: WasmWebClient,
  result: TransactionResult,
  config: ResolvedProverConfig,
  runtime: RetryRuntime = productionRetryRuntime,
): Promise<ProvenTransaction> {
  for (let attempt = 0; attempt < config.maxAttempts; attempt += 1) {
    try {
      const prover = config.createProver();
      return prover === undefined
        ? await client.proveTransaction(result)
        : await client.proveTransaction(result, prover);
    } catch (error) {
      if (!isTransientProverError(error) || attempt + 1 >= config.maxAttempts) {
        throw error;
      }
      await runtime.sleep(retryDelay(attempt, runtime.unitRandom()));
    }
  }

  throw new Error('unreachable prover retry state');
}
