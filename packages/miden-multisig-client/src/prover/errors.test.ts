import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { isTransientProverError } from './errors.js';

interface FixtureError {
  code?: string;
  status?: number;
  message: string;
  cause?: FixtureError;
}

interface Fixtures {
  classifications: Array<{
    name: string;
    chain: FixtureError[];
    transient: boolean;
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

describe('isTransientProverError', () => {
  it('matches every shared classification vector', () => {
    for (const fixture of fixtures().classifications) {
      const error = fixture.chain.reduceRight<FixtureError | undefined>(
        (cause, item) => ({ ...item, cause }),
        undefined,
      );
      expect(isTransientProverError(error), fixture.name).toBe(fixture.transient);
    }
  });

  it('stops safely on cyclic cause graphs', () => {
    const error: FixtureError = { code: 'Unknown', message: 'not retryable' };
    error.cause = error;
    expect(isTransientProverError(error)).toBe(false);
  });

  it('recognizes numeric gRPC status codes', () => {
    expect(isTransientProverError({ code: 14, message: 'unavailable' })).toBe(true);
    expect(isTransientProverError({ code: 3, message: 'timeout text' })).toBe(false);
  });

  it('reads grpc code wording out of plain message text', () => {
    expect(isTransientProverError(new Error('grpc code: NotFound'))).toBe(false);
    expect(
      isTransientProverError(new Error('grpc code: Internal; nested timeout')),
    ).toBe(false);
    expect(isTransientProverError(new Error('grpc code: Unavailable'))).toBe(true);
  });
});
