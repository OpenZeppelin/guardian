import { describe, expect, it } from 'vitest';
import { GuardianHttpError, type ExecutionState, type ProposalExecution } from '@openzeppelin/guardian-client';

import {
  DEFAULT_EXECUTION_WAIT_OPTIONS,
  ExecutionWait,
  GuardianExecutionRefusedError,
  GuardianExecutionWaitTimeoutError,
  LocalExecutionRequiredError,
  ProposalNotHeldLocallyError,
  assertGuardianMayExecute,
  describeLocalExecutionReason,
  localExecutionReason,
  refusingWith,
  type BoundedRead,
  type WaitRuntime,
} from './guardianExecution.js';
import type { ProposalMetadata } from '../types/proposal.js';

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

  /**
   * A scripted read settles at once unless it stalls, and advances the clock by however long it
   * took. One that stalls or takes longer than `budgetMs` ends at the budget, as a real timer would.
   */
  async within<T>(budgetMs: number, pending: Promise<T>): Promise<BoundedRead<T>> {
    const startedMs = this.nowMs;
    const settled = await Promise.race([
      pending.then((value) => ({ kind: 'settled' as const, value })),
      new Promise<null>((resolve) => setTimeout(() => resolve(null), 0)),
    ]);
    if (settled === null || this.nowMs - startedMs > budgetMs) {
      this.nowMs = startedMs + budgetMs;
      return { kind: 'expired' };
    }
    return settled;
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

/** One scripted status read: an immediate answer, an answer after `takesMs`, or one that never answers. */
type Step = Read | { takesMs: number; read: Read } | 'stall';

class ScriptedReads {
  count = 0;
  private readonly script: Step[];

  constructor(
    script: Step[],
    private readonly runtime: FakeRuntime | null = null,
  ) {
    this.script = [...script];
  }

  readonly read = async (): Promise<ProposalExecution> => {
    this.count += 1;
    await Promise.resolve();
    const step = this.script.shift() ?? execution('pending');
    if (step === 'stall') {
      return new Promise<ProposalExecution>(() => {});
    }
    const next = 'takesMs' in step ? this.answerAfter(step.takesMs, step.read) : step;
    if (next instanceof Error) {
      throw next;
    }
    return next;
  };

  private answerAfter(takesMs: number, read: Read): Read {
    if (this.runtime === null) {
      throw new Error('a timed step needs the runtime whose clock it advances');
    }
    this.runtime.nowMs += takesMs;
    return read;
  }
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
    expect(reads.count).toBe(3);
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
    expect(reads.count).toBe(2);
  });

  it('times out a stalled read at the deadline with the last observed execution', async () => {
    const runtime = new FakeRuntime();
    const reads = new ScriptedReads([execution('proving'), 'stall'], runtime);
    const error = await new ExecutionWait('0xprop', options(1, 10, 5))
      .run(reads.read, runtime)
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(GuardianExecutionWaitTimeoutError);
    expect((error as GuardianExecutionWaitTimeoutError).lastObserved?.state).toBe('proving');
    expect(reads.count).toBe(2);
    expect(runtime.nowMs).toBe(5_000);
  });

  it('does not return a terminal answer that arrives after the deadline', async () => {
    const runtime = new FakeRuntime();
    const reads = new ScriptedReads(
      [execution('submitted'), { takesMs: 10_000, read: execution('committed') }],
      runtime,
    );
    const error = await new ExecutionWait('0xprop', options(1, 10, 5))
      .run(reads.read, runtime)
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(GuardianExecutionWaitTimeoutError);
    expect((error as GuardianExecutionWaitTimeoutError).lastObserved?.state).toBe('submitted');
    expect(reads.count).toBe(2);
    expect(runtime.nowMs).toBe(5_000);
  });
});

describe('localExecutionReason', () => {
  const p2id = (noteType: 'public' | 'private' | undefined): { metadata: ProposalMetadata } => ({
    metadata: { proposalType: 'p2id', description: '', recipientId: '0xr', faucetId: '0xf', amount: '1', noteType },
  });
  const switchGuardian: { metadata: ProposalMetadata } = {
    metadata: { proposalType: 'switch_guardian', description: '', newGuardianPubkey: '0xpk' },
  };
  const addSigner: { metadata: ProposalMetadata } = {
    metadata: { proposalType: 'add_signer', description: '', targetThreshold: 1, targetSignerCommitments: ['0xa'] },
  };

  it('names the two proposals GUARDIAN must not execute and nothing else', () => {
    expect(localExecutionReason(switchGuardian)).toBe('switch_guardian');
    expect(localExecutionReason(p2id('private'))).toBe('private_note');
    expect(localExecutionReason(p2id('public'))).toBeNull();
    expect(localExecutionReason(p2id(undefined))).toBeNull();
    expect(localExecutionReason(addSigner)).toBeNull();
  });

  it('refuses a switch even when the request allows a private note', () => {
    const error = (() => {
      try {
        assertGuardianMayExecute('0xprop', switchGuardian, { allowPrivateNote: true });
      } catch (e) {
        return e;
      }
      return null;
    })();
    expect(error).toBeInstanceOf(LocalExecutionRequiredError);
    expect((error as LocalExecutionRequiredError).reason).toBe('switch_guardian');
    expect((error as LocalExecutionRequiredError).message).toContain('executed locally');
    expect((error as LocalExecutionRequiredError).message).toContain(
      describeLocalExecutionReason('switch_guardian'),
    );
  });

  it('refuses a private note unless the request allows it', () => {
    expect(() => assertGuardianMayExecute('0xprop', p2id('private'), {})).toThrow(LocalExecutionRequiredError);
    expect(() => assertGuardianMayExecute('0xprop', p2id('private'), { allowPrivateNote: false })).toThrow(
      LocalExecutionRequiredError,
    );
    expect(() => assertGuardianMayExecute('0xprop', p2id('private'), { allowPrivateNote: true })).not.toThrow();
  });

  it('lets public notes and other proposals through', () => {
    expect(() => assertGuardianMayExecute('0xprop', p2id('public'), {})).not.toThrow();
    expect(() => assertGuardianMayExecute('0xprop', addSigner, {})).not.toThrow();
  });

  it('fails closed on a proposal the client does not hold', () => {
    expect(() => assertGuardianMayExecute('0xprop', undefined, { allowPrivateNote: true })).toThrow(
      ProposalNotHeldLocallyError,
    );
  });
});
