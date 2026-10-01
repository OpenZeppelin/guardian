import { sealTransactionRequest, type TransactionRequestEnvelope } from '@openzeppelin/guardian-client';

/**
 * Whether proposals a client creates can be executed by Guardian. Set once, on the client;
 * the client never asks the server which it offers.
 */
export type ProposalExecutionMode = 'self_executed' | 'guardian_executable';

/** The approval window a Guardian-executable proposal gets when the caller sets none. */
export const GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA = 28_800;

/**
 * The transaction expiration a Guardian-executable request applies, measured from the block
 * it executes against, so a submission's outcome stays within Guardian's resolution horizon.
 */
export const GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA = 256;

/**
 * The `miden-client` version the web SDK embeds, whose serialization a stored request uses.
 * Request bytes carry no version tag of their own, so the server admits them by this name.
 */
export const REQUEST_SERIALIZER_ID = '0.17.0-rc.4';

/** The approval expiration a new proposal applies: the caller's, else this mode's default. */
export function approvalExpirationDeltaFor(
  mode: ProposalExecutionMode,
  requested: number | undefined,
): number | undefined {
  switch (mode) {
    case 'self_executed':
      return requested;
    case 'guardian_executable':
      return requested ?? GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA;
    default: {
      const unreachable: never = mode;
      throw new Error(`unknown execution mode ${String(unreachable)}`);
    }
  }
}

/** The transaction expiration a new proposal's request applies. */
export function transactionExpirationDeltaFor(mode: ProposalExecutionMode): number | undefined {
  switch (mode) {
    case 'self_executed':
      return undefined;
    case 'guardian_executable':
      return GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA;
    default: {
      const unreachable: never = mode;
      throw new Error(`unknown execution mode ${String(unreachable)}`);
    }
  }
}

/**
 * The envelope a new proposal stores its request in, if this mode stores one. The bytes are
 * read only when they are stored.
 */
export async function attachmentFor(
  mode: ProposalExecutionMode,
  requestBytes: () => Uint8Array,
): Promise<TransactionRequestEnvelope | undefined> {
  switch (mode) {
    case 'self_executed':
      return undefined;
    case 'guardian_executable':
      return sealTransactionRequest(requestBytes(), REQUEST_SERIALIZER_ID);
    default: {
      const unreachable: never = mode;
      throw new Error(`unknown execution mode ${String(unreachable)}`);
    }
  }
}

/** The script lines that apply `delta`, placed at the start of a transaction script's `main`. */
export function expirationInstructions(delta: number | undefined): string {
  if (delta === undefined || delta === 0) {
    return '';
  }
  return `push.${delta}\n    exec.::miden::protocol::tx::update_expiration_block_delta\n    `;
}
