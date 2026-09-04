//! Typed failures for auth-arg recovery.
//!
//! The surface lands here deliberately ahead of the code that raises it: nothing in this
//! change constructs these, and the proposal paths that do arrive in the PR that commits
//! the anchored fee faucet. Landing the codes first keeps them out of that PR's diff and
//! lets a caller pin `code` before the raiser exists — `public-api.test.ts` covers the
//! barrel so a dropped re-export is caught either way.

/** Stable error identifiers for auth-arg recovery failures. */
export type AuthArgErrorCode =
  | 'proposal_auth_arg_unresolvable'
  | 'proposal_salt_malformed'
  | 'fee_faucet_anchor_mismatch';

/** How much of an untrusted value an error message will quote. */
const MAX_QUOTED_CHARS = 80;

/**
 * Quotes a GUARDIAN-served value without letting an unbounded string into the
 * message. Control characters would otherwise reach a log verbatim.
 *
 * Applied to every served field these errors interpolate, not only the salt:
 * proposal ids, auth args, faucet ids and the reason text all arrive in the same
 * unvalidated JSON, so sanitising one and not the rest only narrows the hole.
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
function quoteUntrusted(value: unknown): string {
  const asString = coerceForMessage(value);
  const printable = (chunk: string) => chunk.replace(/[^\x20-\x7e]/g, '.');
  return asString.length > MAX_QUOTED_CHARS
    ? `${printable(asString.slice(0, MAX_QUOTED_CHARS))}... (${asString.length} code units)`
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
   * value the message deliberately truncates. The message quotes it already.
   */
  readonly saltHex: unknown;

  constructor(details: { proposalId: string; saltHex: unknown; reason: string; cause?: unknown }) {
    super(
      `Proposal ${quoteUntrusted(details.proposalId)} has a malformed metadata salt ` +
        `'${quoteUntrusted(details.saltHex)}': ${quoteUntrusted(details.reason)}`,
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
 * `switch_guardian` is the exception, and deliberately so: it rebuilds from the
 * summary's own auth arg, which reproduces the signed word but not the fee
 * preimage behind it, so it executes only on a zero-fee chain.
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

  constructor(details: {
    proposalId: string;
    signedAuthArgHex: string;
    saltHex: string;
    feeFaucetIdHex: string;
  }) {
    super(
      `Proposal ${quoteUntrusted(details.proposalId)} auth arg ` +
        `${quoteUntrusted(details.signedAuthArgHex)} is not the fee-conversion commitment to ` +
        `its metadata salt ${quoteUntrusted(details.saltHex)} under fee faucet ` +
        `${quoteUntrusted(details.feeFaucetIdHex)}, so the signed transaction summary cannot ` +
        `be reproduced ` +
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
        `${quoteUntrusted(details.committedFeeFaucetIdHex)} but the anchored block reports ` +
        `${quoteUntrusted(details.anchoredFeeFaucetIdHex)}. Retry the proposal.`,
    );
    this.name = 'FeeFaucetAnchorMismatchError';
    this.committedFeeFaucetIdHex = details.committedFeeFaucetIdHex;
    this.anchoredFeeFaucetIdHex = details.anchoredFeeFaucetIdHex;
  }
}
