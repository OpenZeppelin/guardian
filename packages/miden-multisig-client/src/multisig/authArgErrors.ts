/** Stable error identifiers for auth-arg recovery failures. */
export type AuthArgErrorCode =
  | 'proposal_auth_arg_unresolvable'
  | 'proposal_salt_malformed';

/** How much of an untrusted salt an error message will quote. */
const MAX_QUOTED_SALT_CHARS = 80;

/**
 * Quotes a rejected salt without letting an unbounded metadata string into the
 * message. Control characters would otherwise reach a log verbatim.
 *
 * The value is whatever GUARDIAN served, so it is not necessarily a string and
 * its own `toString` may throw; a value that cannot even be described must not
 * take the error's place, because this error is the one a `switch_guardian`
 * recovers from.
 *
 * Truncation happens before the control characters are stripped, so an
 * oversized value is never copied at full length just to render 80 characters
 * of it.
 */
function quoteSalt(saltHex: unknown): string {
  const asString = coerceForMessage(saltHex);
  const printable = (chunk: string) => chunk.replace(/[^\x20-\x7e]/g, '.');
  return asString.length > MAX_QUOTED_SALT_CHARS
    ? `${printable(asString.slice(0, MAX_QUOTED_SALT_CHARS))}... (${asString.length} code units)`
    : printable(asString);
}

/**
 * Renders an error with the reason underneath it, which for a salt that failed
 * to decode is the SDK's own message and the only place the actual cause
 * appears. Both parts are generated here or by the SDK, so neither is unbounded.
 *
 * The WASM SDK rejects with a bare string rather than an `Error`, so narrowing
 * to `Error` would discard the cause on the one path this exists for.
 */
export function describeWithCause(
  error: ProposalAuthArgUnresolvableError | ProposalSaltMalformedError,
): string {
  const described = error instanceof ProposalAuthArgUnresolvableError ? error.diagnosis : error.message;
  const { cause } = error;
  if (cause === undefined) {
    return described;
  }
  return `${described}: ${cause instanceof Error ? cause.message : coerceForMessage(cause)}`;
}

function coerceForMessage(value: unknown): string {
  if (typeof value === 'string') {
    return value;
  }

  try {
    return String(value);
  } catch {
    return `<undescribable ${typeof value}>`;
  }
}

/**
 * A proposal's recorded salt is not a readable 32-byte word, so no rebuild can
 * use it.
 *
 * Coded for the same reason as {@link ProposalAuthArgUnresolvableError}, and
 * recoverable in the same one place: a `switch_guardian`'s salt is served by the
 * GUARDIAN being switched away from, which can make it unreadable as easily as
 * it can make it wrong. Treating only the latter as recoverable would leave that
 * GUARDIAN able to strand a fully signed switch.
 */
export class ProposalSaltMalformedError extends Error {
  readonly code: AuthArgErrorCode = 'proposal_salt_malformed';
  readonly proposalId: string;
  /**
   * Exactly what GUARDIAN served, so not necessarily a string and not
   * necessarily bounded. Non-enumerable, because the default logging paths
   * (`util.inspect`, `JSON.stringify`) would otherwise re-expose the unbounded
   * value the message deliberately truncates. Use {@link quotedSalt} to print.
   */
  readonly saltHex: unknown;

  constructor(details: { proposalId: string; saltHex: unknown; reason: string; cause?: unknown }) {
    super(
      `Proposal ${details.proposalId} has a malformed metadata salt ` +
        `'${quoteSalt(details.saltHex)}': ${details.reason}`,
      details.cause === undefined ? undefined : { cause: details.cause },
    );
    this.name = 'ProposalSaltMalformedError';
    this.proposalId = details.proposalId;
    Object.defineProperty(this, 'saltHex', {
      value: details.saltHex,
      enumerable: false,
      writable: false,
    });
  }

  /** The bounded, printable form of {@link saltHex}, safe to log. */
  get quotedSalt(): string {
    return quoteSalt(this.saltHex);
  }
}

/**
 * A proposal's signed auth arg is neither its recorded salt nor a fee-conversion
 * commitment to that salt, so no rebuild can reproduce the signed summary.
 *
 * Coded because one caller acts on it rather than reporting it:
 * `switch_guardian` recovery falls back to the summary's own auth arg when it
 * sees this or {@link ProposalSaltMalformedError}, and must not extend that
 * treatment to an unreadable anchor or a WASM failure.
 */
export class ProposalAuthArgUnresolvableError extends Error {
  readonly code: AuthArgErrorCode = 'proposal_auth_arg_unresolvable';
  readonly proposalId: string;
  readonly signedAuthArgHex: string;
  readonly saltHex: string;
  readonly feeFaucetIdHex: string;
  /**
   * What was observed, without the remediation the message appends. The
   * remediation is only true where this is fatal; `switch_guardian` recovers
   * from it and executes the proposal anyway, so its warning quotes this.
   */
  readonly diagnosis: string;

  constructor(details: {
    proposalId: string;
    signedAuthArgHex: string;
    saltHex: string;
    feeFaucetIdHex: string;
  }) {
    const diagnosis =
      `Proposal ${details.proposalId} auth arg ${details.signedAuthArgHex} is neither its ` +
      `metadata salt ${details.saltHex} nor a fee-conversion commitment to that salt under ` +
      `fee faucet ${details.feeFaucetIdHex}, so the signed transaction summary cannot be ` +
      'reproduced from that salt';
    super(
      `${diagnosis}, and the proposal cannot be executed. Recreate the proposal and collect ` +
        'signatures again, and have the original dropped server-side — while GUARDIAN keeps ' +
        'serving it, syncing this account keeps failing on it',
    );
    this.name = 'ProposalAuthArgUnresolvableError';
    this.proposalId = details.proposalId;
    this.signedAuthArgHex = details.signedAuthArgHex;
    this.saltHex = details.saltHex;
    this.feeFaucetIdHex = details.feeFaucetIdHex;
    this.diagnosis = diagnosis;
  }
}
