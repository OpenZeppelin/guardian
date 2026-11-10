/**
 * Version 2 session request envelope (mirror of
 * `crates/shared/src/session_request.rs`). A session key signs a fixed
 * 62-byte envelope that binds the transport and the operation in addition to
 * the account, timestamp and payload of the `AuthRequestMessage` word, so a
 * request signed for one route is never accepted on another:
 *
 * `"GUARDIAN_SESSION_REQUEST" 0x00 || u16be(2) || u8(transport) ||
 * u16be(operation) || request word (32 bytes)`
 *
 * P-256 signs these bytes with SHA-256: WebCrypto
 * `sign({ name: 'ECDSA', hash: 'SHA-256' }, key, envelope)`.
 */

/** `x-auth-format` value of a version 2 session request. */
export const SESSION_REQUEST_AUTH_FORMAT = 'session-v2';

/** The envelope version this client signs. */
export const SESSION_REQUEST_VERSION = 2;

/** Length of an encoded envelope. */
export const SESSION_REQUEST_ENVELOPE_LENGTH = 62;

const DOMAIN = new TextEncoder().encode('GUARDIAN_SESSION_REQUEST\0');

/** How a request reaches Guardian. This client always uses HTTP. */
export const SessionTransport = {
  Http: 1,
  Grpc: 2,
} as const;
export type SessionTransport = (typeof SessionTransport)[keyof typeof SessionTransport];

/** Every per-account operation a session may sign. Ids are permanent. */
export const SessionOperation = {
  GetState: 1,
  GetCanonicalNonce: 2,
  GetDelta: 3,
  GetDeltaSince: 4,
  GetDeltaHistory: 5,
  GetDeltaProposals: 6,
  GetDeltaProposal: 7,
  PushDelta: 8,
  PushDeltaProposal: 9,
  SignDeltaProposal: 10,
  ExecuteDeltaProposal: 11,
  GetDeltaProposalExecution: 12,
  GetCurrentExecution: 13,
} as const;
export type SessionOperation = (typeof SessionOperation)[keyof typeof SessionOperation];

const WRITE_OPERATIONS: ReadonlySet<SessionOperation> = new Set([
  SessionOperation.PushDelta,
  SessionOperation.PushDeltaProposal,
  SessionOperation.SignDeltaProposal,
  SessionOperation.ExecuteDeltaProposal,
]);

/** Whether Guardian checks `operation` under the strict write floor. */
export function isSessionWrite(operation: SessionOperation): boolean {
  return WRITE_OPERATIONS.has(operation);
}

/** The bytes a session key signs for one per-account request. */
export function sessionRequestEnvelope(
  transport: SessionTransport,
  operation: SessionOperation,
  requestWord: Uint8Array
): Uint8Array<ArrayBuffer> {
  if (requestWord.length !== 32) {
    throw new Error(`Request word must be 32 bytes, got ${requestWord.length}`);
  }
  const envelope = new Uint8Array(SESSION_REQUEST_ENVELOPE_LENGTH);
  envelope.set(DOMAIN, 0);
  envelope[25] = (SESSION_REQUEST_VERSION >> 8) & 0xff;
  envelope[26] = SESSION_REQUEST_VERSION & 0xff;
  envelope[27] = transport;
  envelope[28] = (operation >> 8) & 0xff;
  envelope[29] = operation & 0xff;
  envelope.set(requestWord, 30);
  return envelope;
}

/**
 * Guardian does not support the session request format this client signs.
 * Terminal: the session is kept, nothing is retried and the wallet does not
 * sign instead.
 */
export class UnsupportedAuthVersionError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'UnsupportedAuthVersionError';
  }
}

/**
 * Throws {@link UnsupportedAuthVersionError} unless a Guardian reports the
 * session request version this client signs: such a server would refuse
 * every session request.
 */
export function assertSupportedSessionPolicy(policy: { requestAuthVersion?: number }): void {
  if (policy.requestAuthVersion !== SESSION_REQUEST_VERSION) {
    throw new UnsupportedAuthVersionError(
      `Guardian reports session request version ${policy.requestAuthVersion ?? 'none'}, ` +
        `this client signs version ${SESSION_REQUEST_VERSION}`
    );
  }
}

/** How far a session request timestamp may run ahead of the local clock. */
export const SESSION_TIMESTAMP_MAX_AHEAD_MS = 1_000;

/**
 * A next timestamp this far ahead of the local clock means the clock stepped
 * back, so minting restarts from the clock instead of waiting the step out.
 */
export const SESSION_CLOCK_STEP_BACK_MS = 5_000;

/**
 * After a replay rejection the next timestamp jumps to within this much below
 * the ahead limit, at random, so it clears a floor an earlier page or tab of
 * the same session left, and two of them stop colliding.
 */
export const SESSION_REPLAY_JUMP_SPREAD_MS = 200;

/**
 * Strictly increasing request timestamps per account, shared by every client
 * using one session, so two otherwise identical requests are never the same
 * signed bytes. Accounts are independent: a burst on one does not push
 * another's timestamps ahead of the clock.
 */
export class SessionTimestamps {
  private readonly last = new Map<string, number>();

  constructor(private readonly now: () => number = Date.now) {}

  /**
   * The next timestamp for `accountId`, waiting while it would run more than a
   * second ahead. `afterReplay` jumps it near that limit first (see
   * `SESSION_REPLAY_JUMP_SPREAD_MS`).
   */
  async next(accountId: string, afterReplay = false): Promise<number> {
    if (afterReplay) {
      const jump =
        this.now() +
        SESSION_TIMESTAMP_MAX_AHEAD_MS -
        Math.floor(Math.random() * SESSION_REPLAY_JUMP_SPREAD_MS);
      this.last.set(accountId, Math.max(this.last.get(accountId) ?? -Infinity, jump - 1));
    }
    for (;;) {
      const now = this.now();
      let last = this.last.get(accountId) ?? -Infinity;
      if (last - now > SESSION_CLOCK_STEP_BACK_MS) {
        last = now - 1;
      }
      const next = Math.max(now, last + 1);
      const ahead = next - now;
      if (ahead <= SESSION_TIMESTAMP_MAX_AHEAD_MS) {
        this.last.set(accountId, next);
        return next;
      }
      await new Promise((resolve) => setTimeout(resolve, ahead - SESSION_TIMESTAMP_MAX_AHEAD_MS));
    }
  }
}

/**
 * Runs a session's writes to one account one at a time, from minting the
 * timestamp through the last replay retry, so they reach Guardian in
 * timestamp order and never lose the strict write floor to each other. Writes
 * to other accounts and all reads run freely.
 */
export class WriteSequencer {
  private readonly tails = new Map<string, Promise<unknown>>();

  run<T>(accountId: string, write: () => Promise<T>): Promise<T> {
    const previous = this.tails.get(accountId) ?? Promise.resolve();
    const result = previous.then(write);
    const tail = result.then(
      () => undefined,
      () => undefined,
    );
    this.tails.set(accountId, tail);
    void tail.then(() => {
      if (this.tails.get(accountId) === tail) {
        this.tails.delete(accountId);
      }
    });
    return result;
  }
}
