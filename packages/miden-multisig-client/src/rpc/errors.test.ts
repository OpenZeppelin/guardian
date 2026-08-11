import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { isTransientRpcError } from './errors.js';

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
        '../../../../fixtures/miden-multisig-client/rpc-policy-fixtures.json',
        import.meta.url,
      ),
      'utf8',
    ),
  ) as Fixtures;
}

describe('isTransientRpcError', () => {
  it('matches every shared classification vector', () => {
    for (const fixture of fixtures().classifications) {
      const error = fixture.chain.reduceRight<FixtureError | undefined>(
        (cause, item) => ({ ...item, cause }),
        undefined,
      );
      expect(isTransientRpcError(error), fixture.name).toBe(fixture.transient);
    }
  });

  it('stops safely on cyclic cause graphs', () => {
    const error: FixtureError = { code: 'Unknown', message: 'not retryable' };
    error.cause = error;
    expect(isTransientRpcError(error)).toBe(false);
  });

  it('recognizes numeric gRPC status codes', () => {
    expect(isTransientRpcError({ code: 14, message: 'unavailable' })).toBe(true);
    expect(isTransientRpcError({ code: 3, message: 'timeout text' })).toBe(false);
  });

  it('keeps permanent status evidence ahead of transport wording', () => {
    expect(
      isTransientRpcError({
        code: 'FailedPrecondition',
        message: 'transport error while checking preconditions',
      }),
    ).toBe(false);
  });

  it('reads grpc code wording out of plain message text', () => {
    expect(isTransientRpcError(new Error('grpc code: InvalidArgument; request timeout'))).toBe(
      false,
    );
    expect(isTransientRpcError(new Error('grpc code: Unavailable'))).toBe(true);
  });
});
