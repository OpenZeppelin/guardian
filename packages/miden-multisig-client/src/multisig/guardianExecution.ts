import { GuardianHttpError, isTerminalExecutionState, type ProposalExecution } from '@openzeppelin/guardian-client';

/**
 * Guardian refused an execution request or an execution read with a stable code, such as
 * `GUARDIAN_PROPOSAL_NOT_READY` or `GUARDIAN_EXECUTION_CONFLICT`. Mirrors the Rust SDK's
 * `MultisigError::GuardianExecutionRefused`: `code` is the verbatim wire code, `retryable` the
 * server's classification, `retryAfterSecs` its backoff hint and `blockingProposalId` the
 * proposal whose execution holds the account, on a conflict.
 */
export class GuardianExecutionRefusedError extends Error {
  readonly code: string;
  readonly userMessage: string;
  readonly retryable: boolean;
  readonly retryAfterSecs: number | null;
  readonly blockingProposalId: string | null;

  constructor(details: {
    code: string;
    message: string;
    retryable: boolean;
    retryAfterSecs: number | null;
    blockingProposalId: string | null;
    cause?: unknown;
  }) {
    super(`GUARDIAN refused execution (${details.code}): ${details.message}`, { cause: details.cause });
    this.name = 'GuardianExecutionRefusedError';
    this.code = details.code;
    this.userMessage = details.message;
    this.retryable = details.retryable;
    this.retryAfterSecs = details.retryAfterSecs;
    this.blockingProposalId = details.blockingProposalId;
  }

  /** The refusal a Guardian error carries, or the error unchanged when it carries no code. */
  static fromGuardian(error: unknown): unknown {
    if (!(error instanceof GuardianHttpError) || error.rawCode === null) {
      return error;
    }
    return new GuardianExecutionRefusedError({
      code: error.rawCode,
      message: error.userMessage ?? error.message,
      retryable: error.isRetryable(),
      retryAfterSecs: error.retryAfterSecs() ?? null,
      blockingProposalId: error.meta?.blockingProposalId ?? null,
      cause: error,
    });
  }
}

/** Resolves with `pending`, rethrowing a Guardian refusal as {@link GuardianExecutionRefusedError}. */
export async function refusingWith<T>(pending: Promise<T>): Promise<T> {
  try {
    return await pending;
  } catch (error) {
    throw GuardianExecutionRefusedError.fromGuardian(error);
  }
}

/**
 * `waitForGuardianExecution` reached its deadline before the execution finished. The execution
 * itself keeps running. Mirrors the Rust SDK's `MultisigError::GuardianExecutionWaitTimedOut`.
 */
export class GuardianExecutionWaitTimeoutError extends Error {
  readonly proposalId: string;
  readonly deadlineMs: number;
  /** The last execution the wait read, or `null` when no read succeeded. */
  readonly lastObserved: ProposalExecution | null;

  constructor(details: { proposalId: string; deadlineMs: number; lastObserved: ProposalExecution | null }) {
    super(`GUARDIAN execution of proposal ${details.proposalId} did not finish within ${details.deadlineMs} ms`);
    this.name = 'GuardianExecutionWaitTimeoutError';
    this.proposalId = details.proposalId;
    this.deadlineMs = details.deadlineMs;
    this.lastObserved = details.lastObserved;
  }
}

/**
 * How `waitForGuardianExecution` polls. The pause between polls starts at `initialBackoffMs` and
 * doubles after every poll up to `maxBackoffMs`; a retryable read that carries a server retry
 * hint waits for the hint instead. The wait gives up once `deadlineMs` has elapsed since it
 * started. Mirrors the Rust SDK's `ExecutionWaitOptions`.
 */
export interface ExecutionWaitOptions {
  initialBackoffMs?: number;
  maxBackoffMs?: number;
  deadlineMs?: number;
}

export const DEFAULT_EXECUTION_WAIT_OPTIONS: Required<ExecutionWaitOptions> = {
  initialBackoffMs: 1_000,
  maxBackoffMs: 10_000,
  deadlineMs: 15 * 60 * 1_000,
};

/** A read bounded by a time budget: its value, or `expired` when it did not settle in time. */
export type BoundedRead<T> = { kind: 'settled'; value: T } | { kind: 'expired' };

/** The clock, sleep and read bound a wait runs on, injectable so the wait is testable. */
export interface WaitRuntime {
  elapsedMs(): number;
  sleep(delayMs: number): Promise<void>;
  /** Settles with `pending`, or `expired` once `budgetMs` passes first; a rejection propagates. */
  within<T>(budgetMs: number, pending: Promise<T>): Promise<BoundedRead<T>>;
}

export function startWaitRuntime(): WaitRuntime {
  const started = performance.now();
  return {
    elapsedMs: () => performance.now() - started,
    sleep: (delayMs) => new Promise((resolve) => setTimeout(resolve, delayMs)),
    within: <T>(budgetMs: number, pending: Promise<T>) =>
      new Promise<BoundedRead<T>>((resolve, reject) => {
        const timer = setTimeout(() => resolve({ kind: 'expired' }), budgetMs);
        pending.then(
          (value) => {
            clearTimeout(timer);
            resolve({ kind: 'settled', value });
          },
          (error: unknown) => {
            clearTimeout(timer);
            reject(error);
          },
        );
      }),
  };
}

const TRANSPORT_HTTP_STATUSES = new Set([502, 503, 504]);

type ReadRetry = { kind: 'after'; delayMs: number } | { kind: 'backoff' } | { kind: 'never' };

type PollOutcome =
  | { kind: 'finished'; execution: ProposalExecution }
  | { kind: 'pause'; pauseMs: number; observed: ProposalExecution | null }
  | { kind: 'expired' };

/**
 * A failed status read and whether the wait retries it: a Guardian error the server marked
 * retryable, or a transport failure (a fetch that never got a response, or a 502, 503 or 504
 * without a Guardian error body). Matches the Rust SDK's classification of gRPC transport
 * errors and `Unavailable` or `DeadlineExceeded` statuses without a Guardian code.
 */
class StatusReadFailure {
  constructor(readonly error: unknown) {}

  retry(): ReadRetry {
    if (this.error instanceof GuardianHttpError) {
      const transport = this.error.rawCode === null && TRANSPORT_HTTP_STATUSES.has(this.error.status);
      if (!this.error.isRetryable() && !transport) {
        return { kind: 'never' };
      }
      const hint = this.error.retryAfterSecs();
      return hint === undefined ? { kind: 'backoff' } : { kind: 'after', delayMs: hint * 1_000 };
    }
    return this.error instanceof TypeError ? { kind: 'backoff' } : { kind: 'never' };
  }

  toError(): unknown {
    return GuardianExecutionRefusedError.fromGuardian(this.error);
  }

  /** The pause before the next read, or the read's error when it is not worth retrying. */
  static pauseOrThrow(error: unknown, backoffMs: number): number {
    const failure = new StatusReadFailure(error);
    const retry = failure.retry();
    switch (retry.kind) {
      case 'after':
        return retry.delayMs;
      case 'backoff':
        return backoffMs;
      case 'never':
        throw failure.toError();
      default: {
        const unreachable: never = retry;
        throw new Error(`Unknown read retry: ${JSON.stringify(unreachable)}`);
      }
    }
  }
}

/**
 * Polls one proposal's execution until it is terminal or the deadline passes. Every read is
 * bounded by the time left, so a stalled read cannot outlast the deadline and an answer arriving
 * after it is not returned. It only reads: it never asks Guardian to execute.
 */
export class ExecutionWait {
  private readonly options: Required<ExecutionWaitOptions>;

  constructor(
    private readonly proposalId: string,
    options: ExecutionWaitOptions = {},
  ) {
    this.options = { ...DEFAULT_EXECUTION_WAIT_OPTIONS, ...options };
  }

  async run(read: () => Promise<ProposalExecution>, runtime: WaitRuntime): Promise<ProposalExecution> {
    let backoffMs = this.options.initialBackoffMs;
    let lastObserved: ProposalExecution | null = null;
    for (;;) {
      const outcome = await this.poll(read, runtime, backoffMs);
      switch (outcome.kind) {
        case 'finished':
          return outcome.execution;
        case 'expired':
          throw this.timedOut(lastObserved);
        case 'pause':
          break;
        default: {
          const unreachable: never = outcome;
          throw new Error(`Unknown poll outcome: ${JSON.stringify(unreachable)}`);
        }
      }
      lastObserved = outcome.observed ?? lastObserved;
      const remainingMs = this.remainingMs(runtime);
      if (remainingMs === 0) {
        throw this.timedOut(lastObserved);
      }
      await runtime.sleep(Math.min(outcome.pauseMs, remainingMs));
      backoffMs = Math.min(backoffMs * 2, this.options.maxBackoffMs);
    }
  }

  private remainingMs(runtime: WaitRuntime): number {
    return Math.max(0, this.options.deadlineMs - runtime.elapsedMs());
  }

  private timedOut(lastObserved: ProposalExecution | null): GuardianExecutionWaitTimeoutError {
    return new GuardianExecutionWaitTimeoutError({
      proposalId: this.proposalId,
      deadlineMs: this.options.deadlineMs,
      lastObserved,
    });
  }

  private async poll(
    read: () => Promise<ProposalExecution>,
    runtime: WaitRuntime,
    backoffMs: number,
  ): Promise<PollOutcome> {
    const remainingMs = this.remainingMs(runtime);
    if (remainingMs === 0) {
      return { kind: 'expired' };
    }
    let bounded: BoundedRead<ProposalExecution>;
    try {
      bounded = await runtime.within(remainingMs, read());
    } catch (error) {
      return { kind: 'pause', pauseMs: StatusReadFailure.pauseOrThrow(error, backoffMs), observed: null };
    }
    if (bounded.kind === 'expired') {
      return { kind: 'expired' };
    }
    const execution = bounded.value;
    return isTerminalExecutionState(execution.state)
      ? { kind: 'finished', execution }
      : { kind: 'pause', pauseMs: backoffMs, observed: execution };
  }
}
