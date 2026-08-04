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
