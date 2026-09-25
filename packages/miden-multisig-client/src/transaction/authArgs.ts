import type { Felt, TransactionRequest, TransactionRequestBuilder, Word } from '@miden-sdk/miden-sdk';
import { AccountId, Word as WordType } from '@miden-sdk/miden-sdk';
import { MultisigAuthArgsMissingError } from '../multisig/authArgErrors.js';
import { getRawMidenClient, isPublicMidenClient, type RawClientSource } from '../raw-client.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { randomWord } from '../utils/random.js';
import type { MultisigRequestOptions } from './options.js';

/**
 * Layout of the multisig auth-args preimage the auth arg commits to, in felts:
 * `[bound_block_num, approval_expiration_block_num, 0, 0, SALT, CONVERSION_INFO]`.
 */
const AUTH_ARGS_BOUND_BLOCK_INDEX = 0;
const AUTH_ARGS_SALT_OFFSET = 4;
const AUTH_ARGS_NUM_ELEMENTS = 12;

/**
 * A request under construction that already carries the multisig auth args,
 * and the salt they commit to as normalized hex.
 */
export interface MultisigRequestDraft {
  builder: TransactionRequestBuilder;
  saltHex: string;
}

/**
 * Starts a request carrying the multisig auth args for `options.accountId`:
 * the three-word preimage in the advice map and its commitment as the auth
 * arg, which miden-client then leaves alone.
 *
 * The salt is `options.salt` or a fresh one. `approvalExpirationDelta` left
 * out means the approval never expires, the upstream default. `boundBlockNum`
 * left out binds the store's sync height, which is what a proposer wants; a
 * rebuild pins the proposal's anchor block and the expiration the summary
 * already binds.
 *
 * The salt is moved across the WASM boundary, so a handle is built here from
 * the hex rather than taken from the caller, and nothing frees it afterwards.
 */
export async function multisigRequestBuilder(
  client: RawClientSource,
  options: MultisigRequestOptions,
): Promise<MultisigRequestDraft> {
  const { accountId, boundBlockNum, approvalExpirationDelta, midenRpcEndpoint } = options;
  assertApprovalExpirationDelta(approvalExpirationDelta);
  const saltHex = normalizeHexWord(options.salt ? options.salt.toHex() : randomWord().toHex());
  const salt = WordType.fromHex(saltHex);
  if (isPublicMidenClient(client)) {
    const builder = await client.feeAwareTransactionRequestBuilder(accountId, {
      feeConversionSalt: salt,
      boundBlockNum,
      approvalExpirationDelta,
    });
    return { builder, saltHex };
  }
  const rawClient = await getRawMidenClient(client);
  const builder = await rawClient.feeAwareTransactionRequestBuilder(
    AccountId.fromHex(accountId),
    approvalExpirationDelta ?? null,
    salt,
    boundBlockNum ?? null,
  );
  return { builder, saltHex };
}

/**
 * The furthest a transaction may expire after its reference block
 * (`MAX_EXPIRATION_BLOCK_DELTA` in the transaction kernel). The auth procedure
 * clamps the approval expiration it applies to this, so a longer approval would
 * outlive the transaction it authorizes: the summary would still say "valid"
 * while the node already refuses the submission as expired.
 */
export const MAX_APPROVAL_EXPIRATION_DELTA = 65_535;

/**
 * Zero is not "no expiration": the kernel reads it as expired at the bound
 * block and rejects the transaction. Refused here, where the caller can see why,
 * together with a delta the auth procedure would silently clamp.
 */
function assertApprovalExpirationDelta(delta: number | undefined): void {
  if (delta === undefined) {
    return;
  }
  if (!Number.isInteger(delta) || delta < 1 || delta > MAX_APPROVAL_EXPIRATION_DELTA) {
    throw new Error(
      `approvalExpirationDelta must be a whole number of blocks between 1 and ${MAX_APPROVAL_EXPIRATION_DELTA}, ` +
        `got ${delta}; omit it for an approval that does not expire`,
    );
  }
}

/**
 * Builds the request and hands back the salt it commits to as a fresh handle
 * the caller owns. Refuses a request that carries no auth arg:
 * `feeAwareTransactionRequestBuilder` hands back an untouched builder for an
 * account it cannot classify as a multisig, typically one the client's store
 * does not hold, and such a request would only fail later, inside the VM,
 * while the auth procedure pipes a preimage that is not there.
 */
export function buildMultisigRequest(
  builder: TransactionRequestBuilder,
  saltHex: string,
  accountId: string,
): { request: TransactionRequest; salt: Word } {
  const request = builder.build();
  if (!request.authArg()) {
    throw new MultisigAuthArgsMissingError(accountId);
  }
  return { request, salt: WordType.fromHex(saltHex) };
}

/**
 * The block a multisig request's summary binds, read from the auth-args
 * preimage the request carries. `undefined` when the request carries none.
 */
export function requestBoundBlockNum(request: TransactionRequest): number | undefined {
  const preimage = authArgsPreimage(request);
  return preimage ? Number(preimage[AUTH_ARGS_BOUND_BLOCK_INDEX].asInt()) : undefined;
}

/**
 * The salt a multisig request's summary binds, read from the auth-args
 * preimage the request carries. `undefined` when the request carries none.
 */
export function requestSaltHex(request: TransactionRequest): string | undefined {
  const preimage = authArgsPreimage(request);
  if (!preimage) {
    return undefined;
  }
  return WordType.newFromFelts(
    preimage.slice(AUTH_ARGS_SALT_OFFSET, AUTH_ARGS_SALT_OFFSET + 4),
  ).toHex();
}

function authArgsPreimage(request: TransactionRequest): Felt[] | undefined {
  const authArg = request.authArg();
  if (!authArg) {
    return undefined;
  }
  const felts = request.adviceMap().get(authArg);
  return felts && felts.length === AUTH_ARGS_NUM_ELEMENTS ? felts : undefined;
}
