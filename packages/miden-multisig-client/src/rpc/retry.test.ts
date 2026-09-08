import { readFileSync } from 'node:fs';
import { describe, expect, it, vi } from 'vitest';
import { resolveRpcConfig } from './config.js';
import { retryRpcRead } from './retry.js';
import type { RetryRuntime } from '../retry/runtime.js';
import { retryDelay } from '../retry/runtime.js';

interface Fixtures {
  delays: Array<{ retryIndex: number; unitRandom: number; delayMs: number }>;
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

function recordingRuntime(): RetryRuntime & { sleeps: number[] } {
  const sleeps: number[] = [];
  return {
    sleeps,
    sleep: (delayMs) => {
      sleeps.push(delayMs);
      return Promise.resolve();
    },
    unitRandom: () => 0.5,
  };
}

function rateLimitError(): Error {
  return Object.assign(new Error('Too Many Requests!'), {
    code: 'ResourceExhausted',
  });
}

describe('retryRpcRead', () => {
  it('matches every shared delay vector', () => {
    for (const fixture of fixtures().delays) {
      expect(retryDelay(fixture.retryIndex, fixture.unitRandom)).toBe(fixture.delayMs);
    }
  });

  it('waits out rate limiting and returns the eventual result', async () => {
    const runtime = recordingRuntime();
    const operation = vi
      .fn()
      .mockRejectedValueOnce(rateLimitError())
      .mockRejectedValueOnce(rateLimitError())
      .mockResolvedValueOnce('synced');

    const result = await retryRpcRead(
      operation,
      resolveRpcConfig({ retry: { maxAttempts: 3 } }),
      runtime,
    );

    expect(result).toBe('synced');
    expect(operation).toHaveBeenCalledTimes(3);
    expect(runtime.sleeps).toEqual([retryDelay(0, 0.5), retryDelay(1, 0.5)]);
  });

  it('throws a permanent error immediately without sleeping', async () => {
    const runtime = recordingRuntime();
    const permanent = Object.assign(new Error('malformed account id'), {
      code: 'InvalidArgument',
    });
    const operation = vi.fn().mockRejectedValue(permanent);

    await expect(
      retryRpcRead(operation, resolveRpcConfig({ retry: { maxAttempts: 5 } }), runtime),
    ).rejects.toBe(permanent);
    expect(operation).toHaveBeenCalledTimes(1);
    expect(runtime.sleeps).toEqual([]);
  });

  it('rethrows the final upstream error unchanged after budget exhaustion', async () => {
    const runtime = recordingRuntime();
    const final = Object.assign(new Error('still rate limited'), {
      code: 'ResourceExhausted',
    });
    const operation = vi
      .fn()
      .mockRejectedValueOnce(rateLimitError())
      .mockRejectedValueOnce(final);

    await expect(
      retryRpcRead(operation, resolveRpcConfig({ retry: { maxAttempts: 2 } }), runtime),
    ).rejects.toBe(final);
    expect(operation).toHaveBeenCalledTimes(2);
    expect(runtime.sleeps).toHaveLength(1);
  });

  it('retries a transient read once under the default config', async () => {
    const runtime = recordingRuntime();
    const operation = vi
      .fn()
      .mockRejectedValueOnce(rateLimitError())
      .mockResolvedValueOnce('synced');

    await expect(retryRpcRead(operation, resolveRpcConfig(undefined), runtime)).resolves.toBe(
      'synced',
    );
    expect(operation).toHaveBeenCalledTimes(2);
    expect(runtime.sleeps).toHaveLength(1);
  });

  it('never retries under an explicit single-attempt config', async () => {
    const runtime = recordingRuntime();
    const operation = vi.fn().mockRejectedValue(rateLimitError());

    await expect(
      retryRpcRead(operation, resolveRpcConfig({ retry: { maxAttempts: 1 } }), runtime),
    ).rejects.toThrow('Too Many Requests!');
    expect(operation).toHaveBeenCalledTimes(1);
    expect(runtime.sleeps).toEqual([]);
  });

  it('completes 64 concurrent reads against a rate-limited node', async () => {
    const runtime = recordingRuntime();
    let rejected = 0;
    const operation = () => {
      if (rejected < 40) {
        rejected += 1;
        return Promise.reject(rateLimitError());
      }
      return Promise.resolve('synced');
    };

    const results = await Promise.all(
      Array.from({ length: 64 }, () =>
        retryRpcRead(operation, resolveRpcConfig({ retry: { maxAttempts: 4 } }), runtime),
      ),
    );

    expect(results).toEqual(Array.from({ length: 64 }, () => 'synced'));
    expect(rejected).toBe(40);
  });
});
