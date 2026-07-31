import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { retryDelay } from './retry.js';

interface Fixtures {
  delays: Array<{ retryIndex: number; unitRandom: number; delayMs: number }>;
}

function fixtures(): Fixtures {
  return JSON.parse(
    readFileSync(
      new URL(
        '../../../../fixtures/miden-multisig-client/prover-policy-fixtures.json',
        import.meta.url,
      ),
      'utf8',
    ),
  ) as Fixtures;
}

describe('retryDelay', () => {
  it('matches every shared delay vector', () => {
    for (const fixture of fixtures().delays) {
      expect(retryDelay(fixture.retryIndex, fixture.unitRandom)).toBe(fixture.delayMs);
    }
  });

  it('remains capped after numeric overflow', () => {
    expect(retryDelay(Number.MAX_SAFE_INTEGER, 0.5)).toBe(8_000);
  });

  it.each([Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY])(
    'uses neutral jitter for non-finite randomness',
    (unitRandom) => {
      expect(retryDelay(0, unitRandom)).toBe(500);
    },
  );
});
