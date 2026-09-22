//! Typed failures a caller branches on rather than reports: each means the proposal or
//! request itself is unusable, as against a transport or WASM failure a retry might clear.
//! `public-api.test.ts` covers the barrel so a dropped re-export is caught.

/** Stable error identifiers for auth-arg recovery failures. */
export type AuthArgErrorCode = 'proposal_salt_malformed' | 'multisig_auth_args_missing';

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
 * Coded because `switch_guardian` recovery acts on it rather than reporting it:
 * the salt is served by the GUARDIAN being switched away from, which can make it
 * unreadable, and treating that as fatal would leave that GUARDIAN able to strand
 * a fully signed switch.
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
 * A request built for `accountId` came back without the multisig auth args.
 * `feeAwareTransactionRequestBuilder` only attaches them to an account it can
 * classify as a multisig, so this means the account is not in the client's
 * store or its code is not the guarded-multisig component this client knows.
 * Raised at build time: the alternative is an abort inside the auth procedure
 * while it pipes a preimage the advice map does not hold.
 */
export class MultisigAuthArgsMissingError extends Error {
  readonly code: AuthArgErrorCode = 'multisig_auth_args_missing';
  readonly accountId: string;

  constructor(accountId: string) {
    super(
      `Account ${quoteUntrusted(accountId)} received no multisig auth args: the client does ` +
        'not hold it as a guarded-multisig account, so a request built for it cannot be ' +
        'authenticated. Import or create the account in this client first',
    );
    this.name = 'MultisigAuthArgsMissingError';
    this.accountId = accountId;
  }
}
