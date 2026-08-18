import { readFileSync } from 'node:fs';
import type { TransactionProver } from '@miden-sdk/miden-sdk';
import { describe, expect, it } from 'vitest';
import { resolveProverConfig } from './config.js';

interface Fixtures {
  attemptBudgets: Array<{ input: number | null; normalized: number }>;
  endpoints: Array<{
    input: string;
    valid: boolean;
    canonical?: string;
  }>;
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

describe('resolveProverConfig', () => {
  it('matches shared URL vectors', () => {
    for (const fixture of fixtures().endpoints) {
      const resolve = () => resolveProverConfig({ url: fixture.input }, null);
      if (fixture.valid) {
        expect(resolve().url, fixture.input).toBe(fixture.canonical);
      } else {
        expect(resolve, fixture.input).toThrow();
      }
    }
  });

  it('matches shared attempt-budget vectors for remote proving', () => {
    for (const fixture of fixtures().attemptBudgets) {
      const retry = fixture.input === null ? undefined : { maxAttempts: fixture.input };
      const resolved = resolveProverConfig(
        { url: 'https://prover.example', retry },
        null,
      );
      expect(resolved.maxAttempts).toBe(fixture.normalized);
    }
  });

  it.each([-1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, 4_294_967_296])(
    'rejects invalid maxAttempts value %s',
    (maxAttempts) => {
      expect(() =>
        resolveProverConfig(
          { url: 'https://prover.example', retry: { maxAttempts } },
          null,
        ),
      ).toThrow('prover.retry.maxAttempts');
    },
  );

  it('keeps an endpoint-less injected prover at one attempt without serializing it', () => {
    const serialize = () => {
      throw new Error('callback provers cannot be serialized');
    };
    const callback = {
      endpoint: () => undefined,
      serialize,
    } as unknown as TransactionProver;

    expect(resolveProverConfig({ retry: { maxAttempts: 5 } }, callback)).toMatchObject({
      kind: 'injected',
      maxAttempts: 1,
    });
  });

  it('lets a custom remote URL override an injected local prover', () => {
    const local = {
      serialize: () => 'local',
      endpoint: () => undefined,
    } as unknown as TransactionProver;
    expect(
      resolveProverConfig({ url: 'https://prover.example' }, local),
    ).toMatchObject({
      kind: 'remote',
      url: 'https://prover.example/',
      maxAttempts: 2,
    });
  });
});
