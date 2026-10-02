import { describe, expect, it } from 'vitest';
import { GuardianHttpError, type ExecutionState, type ProposalExecution } from '@openzeppelin/guardian-client';

import {
  DEFAULT_EXECUTION_WAIT_OPTIONS,
  ExecutionWait,
  GuardianExecutionRefusedError,
  GuardianExecutionWaitTimeoutError,
  refusingWith,
  type WaitRuntime,
} from './guardianExecution.js';

class FakeRuntime implements WaitRuntime {
  nowMs = 0;
  readonly sleepsMs: number[] = [];

  elapsedMs(): number {
    return this.nowMs;
  }

  async sleep(delayMs: number): Promise<void> {
    this.nowMs += delayMs;
    this.sleepsMs.push(delayMs);
  }
}

function execution(state: ExecutionState): ProposalExecution {
  return {
    accountId: '0xacc',
    proposalId: '0xprop',
    state,
    error: state === 'failed' ? { code: 'GUARDIAN_EXECUTION_PROVING_FAILED', message: 'proving failed' } : null,
    deltaNonce: null,
    newlyAccepted: false,
    proposalExists: true,
    ignoredSignatures: 0,
    updatedAt: '2026-10-02T00:00:00Z',
  };
}

function guardianError(status: number, body: unknown, retryAfter: string | null = null): GuardianHttpError {
  return new GuardianHttpError(status, 'error', typeof body === 'string' ? body : JSON.stringify(body), retryAfter);
}

type Read = ProposalExecution | Error;

class ScriptedReads {
  count = 0;
  private readonly script: Read[];

  constructor(script: Read[]) {
    this.script = [...script];
  }

  readonly read = async (): Promise<ProposalExecution> => {
    this.count += 1;
    const next = this.script.shift() ?? execution('pending');
    if (next instanceof Error) {
      throw next;
    }
    return next;
  };
}

function options(initialSecs: number, maxSecs: number, deadlineSecs: number) {
  return { initialBackoffMs: initialSecs * 1_000, maxBackoffMs: maxSecs * 1_000, deadlineMs: deadlineSecs * 1_000 };
}

describe('GuardianExecutionRefusedError', () => {
  it('carries the wire code, the retry classification and the blocking proposal of a conflict', async () => {
    const conflict = guardianError(409, {
      code: 'GUARDIAN_EXECUTION_CONFLICT',
      message: 'another proposal is executing',
      meta: { retryable: false, blocking_proposal_id: '0xabc' },
    });
    const error = await refusingWith(Promise.reject(conflict)).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(GuardianExecutionRefusedError);
    const refusal = error as GuardianExecutionRefusedError;
    expect(refusal.code).toBe('GUARDIAN_EXECUTION_CONFLICT');
    expect(refusal.retryable).toBe(false);
    expect(refusal.retryAfterSecs).toBeNull();
    expect(refusal.blockingProposalId).toBe('0xabc');
    expect(refusal.cause).toBe(conflict);
  });

  it('carries the retry hint of a busy refusal and names no proposal', () => {
    const refusal = GuardianExecutionRefusedError.fromGuardian(
      guardianError(409, {
        code: 'GUARDIAN_EXECUTION_BUSY',
        message: 'the account is busy',
        meta: { retryable: true, retry_after_secs: 3 },
      }),
    ) as GuardianExecutionRefusedError;
    expect(refusal.retryable).toBe(true);
    expect(refusal.retryAfterSecs).toBe(3);
    expect(refusal.blockingProposalId).toBeNull();
  });

  it('leaves an error without a Guardian code unchanged', () => {
    const plain = guardianError(502, 'bad gateway');
    expect(GuardianExecutionRefusedError.fromGuardian(plain)).toBe(plain);
  });
});

describe('ExecutionWait', () => {
  it('defaults to one second, ten seconds and fifteen minutes', () => {
    expect(DEFAULT_EXECUTION_WAIT_OPTIONS).toEqual({
      initialBackoffMs: 1_000,
      maxBackoffMs: 10_000,
      deadlineMs: 900_000,
    });
  });

  it('returns a committed execution after backing off between polls', async () => {
    const runtime = new FakeRuntime();
    const reads = new ScriptedReads([
      execution('pending'),
      execution('proving'),
      execution('submitted'),
      execution('committed'),
    ]);
    const result = await new ExecutionWait('0xprop', options(1, 10, 900)).run(reads.read, runtime);
    expect(result.state).toBe('committed');
    expect(reads.count).toBe(4);
    expect(runtime.sleepsMs).toEqual([1_000, 2_000, 4_000]);
  });

  it('returns a failed execution without waiting', async () => {
    const runtime = new FakeRuntime();
    const reads = new ScriptedReads([execution('failed')]);
    const result = await new ExecutionWait('0xprop', options(1, 10, 900)).run(reads.read, runtime);
    expect(result.state).toBe('failed');
    expect(result.error).not.toBeNull();
    expect(reads.count).toBe(1);
    expect(runtime.sleepsMs).toEqual([]);
  });

  it('retries transient reads and honours the retry hint', async () => {
    const runtime = new FakeRuntime();
    const reads = new ScriptedReads([
      guardianError(503, 'upstream unavailable'),
      guardianError(429, { code: 'rate_limit_exceeded', message: 'slow down', meta: { retryable: true } }, '7'),
      new TypeError('fetch failed'),
      execution('committed'),
    ]);
    const result = await new ExecutionWait('0xprop', options(1, 10, 900)).run(reads.read, runtime);
    expect(result.state).toBe('committed');
    expect(reads.count).toBe(4);
    expect(runtime.sleepsMs).toEqual([1_000, 7_000, 4_000]);
  });

  it('throws a non-retryable read error as a refusal', async () => {
    const runtime = new FakeRuntime();
    const reads = new ScriptedReads([
      guardianError(404, {
        code: 'GUARDIAN_EXECUTION_NOT_FOUND',
        message: 'not asked',
        meta: { retryable: false },
      }),
    ]);
    const error = await new ExecutionWait('0xprop', options(1, 10, 900))
      .run(reads.read, runtime)
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(GuardianExecutionRefusedError);
    expect((error as GuardianExecutionRefusedError).code).toBe('GUARDIAN_EXECUTION_NOT_FOUND');
    expect(reads.count).toBe(1);
  });

  it('throws an unclassified error unchanged', async () => {
    const decodeFailure = new Error('Guardian returned an unknown execution state: "paused"');
    const reads = new ScriptedReads([decodeFailure]);
    const error = await new ExecutionWait('0xprop', options(1, 10, 900))
      .run(reads.read, new FakeRuntime())
      .catch((e: unknown) => e);
    expect(error).toBe(decodeFailure);
  });

  it('times out at the deadline with the last observed execution', async () => {
    const runtime = new FakeRuntime();
    const reads = new ScriptedReads([execution('proving')]);
    const error = await new ExecutionWait('0xprop', options(1, 2, 5))
      .run(reads.read, runtime)
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(GuardianExecutionWaitTimeoutError);
    const timeout = error as GuardianExecutionWaitTimeoutError;
    expect(timeout.proposalId).toBe('0xprop');
    expect(timeout.deadlineMs).toBe(5_000);
    expect(timeout.lastObserved?.state).toBe('pending');
    expect(reads.count).toBe(4);
    expect(runtime.sleepsMs).toEqual([1_000, 2_000, 2_000]);
  });

  it('times out without an observation when every read fails', async () => {
    const runtime = new FakeRuntime();
    const reads = new ScriptedReads([new TypeError('fetch failed'), new TypeError('fetch failed'), new TypeError('fetch failed')]);
    const error = await new ExecutionWait('0xprop', options(1, 1, 2))
      .run(reads.read, runtime)
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(GuardianExecutionWaitTimeoutError);
    expect((error as GuardianExecutionWaitTimeoutError).lastObserved).toBeNull();
    expect(reads.count).toBe(3);
  });
});
