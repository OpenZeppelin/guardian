//! Typed failures a caller branches on rather than reports: each means the proposal or
//! request itself is unusable, as against a transport or WASM failure a retry might clear.
//! `public-api.test.ts` covers the barrel so a dropped re-export is caught.

/** Stable error identifiers for auth-arg recovery failures. */
export type AuthArgErrorCode =
  | 'proposal_salt_malformed'
  | 'multisig_auth_args_missing'
  | 'bound_block_not_declared'
  | 'transaction_summary_layout_unsupported'
  | 'bound_block_mismatch';

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

/**
 * A multisig request does not list the block its auth args bind among the
 * blocks it declares through `withBlockNumbers`. A proposal executes at the
 * chain tip, where the auth procedure can read the bound block only from the
 * transaction's partial blockchain, so such a request fails in the VM with
 * `failed to lookup value in Merkle store` once the chain moves past that
 * block. `feeAwareTransactionRequestBuilder` declares it; a request whose auth
 * args are attached by hand has to call `withBlockNumbers([boundBlockNum])`.
 */
export class BoundBlockNotDeclaredError extends Error {
  readonly code: AuthArgErrorCode = 'bound_block_not_declared';
  readonly boundBlockNum: number;

  constructor(boundBlockNum: number) {
    super(
      `The transaction request binds block ${boundBlockNum} in its multisig auth args but does ` +
        'not declare it, so it cannot execute at a later chain tip. Build it with ' +
        `feeAwareTransactionRequestBuilder or add withBlockNumbers([${boundBlockNum}])`,
    );
    this.name = 'BoundBlockNotDeclaredError';
    this.boundBlockNum = boundBlockNum;
  }
}

/**
 * A serialized transaction summary does not have the layout this client reads
 * its bound block from: the version 1 encoding of protocol 0.17. Raised instead
 * of reading a number from the wrong offset, so a protocol upgrade that moves
 * the field fails loudly.
 */
export class TransactionSummaryLayoutError extends Error {
  readonly code: AuthArgErrorCode = 'transaction_summary_layout_unsupported';

  constructor(reason: string) {
    super(`The transaction summary does not have the layout this client reads: ${reason}`);
    this.name = 'TransactionSummaryLayoutError';
  }
}

/**
 * A proposal's `boundBlockNum` names a block other than the one its signed
 * summary binds. The value is served unsigned, so one that disagrees is refused
 * by name for every proposal type, as the Rust SDK does.
 */
export class BoundBlockMismatchError extends Error {
  readonly code: AuthArgErrorCode = 'bound_block_mismatch';
  readonly proposalId: string;
  readonly declaredBoundBlockNum: number;
  readonly boundBlockNum: number;

  constructor(details: { proposalId: string; declaredBoundBlockNum: number; boundBlockNum: number }) {
    super(
      `Proposal ${quoteUntrusted(details.proposalId)} declares boundBlockNum ` +
        `${details.declaredBoundBlockNum}, but its signed transaction summary binds block ` +
        `${details.boundBlockNum}`,
    );
    this.name = 'BoundBlockMismatchError';
    this.proposalId = details.proposalId;
    this.declaredBoundBlockNum = details.declaredBoundBlockNum;
    this.boundBlockNum = details.boundBlockNum;
  }
}
