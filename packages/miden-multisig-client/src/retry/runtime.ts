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
  const finiteRandom = Number.isFinite(unitRandom) ? unitRandom : 0.5;
  const boundedRandom = Math.min(Math.max(finiteRandom, 0), 1 - Number.EPSILON);
  return Math.min(Math.floor(raw * (0.75 + boundedRandom * 0.5)), MAX_DELAY_MS);
}

/**
 * Runs `operation` under an attempt budget: the budget is consulted before
 * the error is classified, transient failures back off with `retryDelay`,
 * and permanent failures or the final attempt rethrow the error unchanged.
 */
export async function retryTransient<T>(
  operation: () => Promise<T>,
  maxAttempts: number,
  isTransient: (error: unknown) => boolean,
  runtime: RetryRuntime,
): Promise<T> {
  for (let attempt = 0; attempt < maxAttempts; attempt += 1) {
    try {
      return await operation();
    } catch (error) {
      if (attempt + 1 >= maxAttempts || !isTransient(error)) {
        throw error;
      }
      await runtime.sleep(retryDelay(attempt, runtime.unitRandom()));
    }
  }

  throw new Error('unreachable retry state');
}
