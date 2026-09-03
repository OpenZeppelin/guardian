/** Stable error identifiers for auth-arg recovery failures. */
export type AuthArgErrorCode =
  | 'proposal_auth_arg_unresolvable'
  | 'proposal_salt_malformed'
  | 'fee_faucet_anchor_mismatch';

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
 * Coded for the same reason as {@link ProposalAuthArgUnresolvableError}: the
 * salt arrives as GUARDIAN-served JSON that is cast rather than validated, so a
 * caller has to be able to tell a bad proposal from a transport or WASM failure
 * without matching on the message.
 */
export class ProposalSaltMalformedError extends Error {
  readonly code: AuthArgErrorCode = 'proposal_salt_malformed';
  readonly proposalId: string;
  /**
   * Exactly what GUARDIAN served, so not necessarily a string and not
   * necessarily bounded. Non-enumerable, because the default logging paths
   * (`util.inspect`, `JSON.stringify`) would otherwise re-expose the unbounded
   * value the message deliberately truncates. The message quotes it already.
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
}

/**
 * A proposal's signed auth arg is not the fee-conversion commitment its recorded
 * salt and anchored fee faucet derive, so no reconstruction from that salt
 * succeeds. Raised before the rebuild, so the proposal fails on the value that is
 * actually wrong rather than on an `ERR_FEE_CONVERSION_INFO_MISSING` abort at
 * proving.
 *
 * Coded so a caller can tell this from a transport or WASM failure: this one says
 * the proposal itself is dead and names what has to be recreated.
 */
export class ProposalAuthArgUnresolvableError extends Error {
  readonly code: AuthArgErrorCode = 'proposal_auth_arg_unresolvable';
  readonly proposalId: string;
  readonly signedAuthArgHex: string;
  readonly saltHex: string;
  readonly feeFaucetIdHex: string;

  constructor(details: {
    proposalId: string;
    signedAuthArgHex: string;
    saltHex: string;
    feeFaucetIdHex: string;
  }) {
    super(
      `Proposal ${details.proposalId} auth arg ${details.signedAuthArgHex} is not the ` +
        `fee-conversion commitment to its metadata salt ${details.saltHex} under fee faucet ` +
        `${details.feeFaucetIdHex}, so the signed transaction summary cannot be reproduced ` +
        'from that salt, and the proposal cannot be executed. Recreate the proposal and ' +
        'collect signatures again, and have the original dropped server-side — while ' +
        'GUARDIAN keeps serving it, syncing this account keeps failing on it',
    );
    this.name = 'ProposalAuthArgUnresolvableError';
    this.proposalId = details.proposalId;
    this.signedAuthArgHex = details.signedAuthArgHex;
    this.saltHex = details.saltHex;
    this.feeFaucetIdHex = details.feeFaucetIdHex;
  }
}

/**
 * The chain's fee faucet changed while a proposal was being built, so the
 * faucet its auth arg committed to is not the one its anchored block reports.
 *
 * Raised at creation, before the proposal is offered for signing. Nobody could
 * rebuild such a proposal — every rebuild derives the faucet from the anchor
 * while the auth arg commits the other one — so it is refused rather than
 * allowed to collect signatures it can never execute with.
 *
 * Coded because it is transient and the remedy is mechanical: build the
 * proposal again against the now-current faucet. Callers that retry should do
 * so on this type rather than by matching the message.
 */
export class FeeFaucetAnchorMismatchError extends Error {
  readonly code: AuthArgErrorCode = 'fee_faucet_anchor_mismatch';
  readonly committedFeeFaucetIdHex: string;
  readonly anchoredFeeFaucetIdHex: string;

  constructor(details: { committedFeeFaucetIdHex: string; anchoredFeeFaucetIdHex: string }) {
    super(
      `Fee faucet moved while the proposal was being built: committed ` +
        `${details.committedFeeFaucetIdHex} but the anchored block reports ` +
        `${details.anchoredFeeFaucetIdHex}. Retry the proposal.`,
    );
    this.name = 'FeeFaucetAnchorMismatchError';
    this.committedFeeFaucetIdHex = details.committedFeeFaucetIdHex;
    this.anchoredFeeFaucetIdHex = details.anchoredFeeFaucetIdHex;
  }
}
