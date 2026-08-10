import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { resolveRpcConfig } from './config.js';

interface Fixtures {
  attemptBudgets: Array<{ input: number | null; normalized: number }>;
}

function fixtures(): Fixtures {
  return JSON.parse(
    readFileSync(
      new URL(
        '../../../../fixtures/miden-multisig-client/rpc-policy-fixtures.json',
        import.meta.url,
      ),
      'utf8',
    ),
  ) as Fixtures;
}

describe('resolveRpcConfig', () => {
  it('matches shared attempt-budget vectors', () => {
    for (const fixture of fixtures().attemptBudgets) {
      const retry = fixture.input === null ? undefined : { maxAttempts: fixture.input };
      expect(resolveRpcConfig({ retry }).maxAttempts).toBe(fixture.normalized);
    }
  });

  it('defaults to one retry when no config is given', () => {
    expect(resolveRpcConfig(undefined).maxAttempts).toBe(2);
  });

  it('treats an explicit single attempt as a full opt-out', () => {
    expect(resolveRpcConfig({ retry: { maxAttempts: 1 } }).maxAttempts).toBe(1);
  });

  it.each([-1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, 4_294_967_296])(
    'rejects invalid maxAttempts value %s',
    (maxAttempts) => {
      expect(() => resolveRpcConfig({ retry: { maxAttempts } })).toThrow(
        'rpc.retry.maxAttempts',
      );
    },
  );
});
