import type { TransactionRequestBuilder, Word } from '@miden-sdk/miden-sdk';
import {
  AccountId,
  AdviceMap,
  Felt,
  FeltArray,
  Poseidon2,
  Word as WordType,
} from '@miden-sdk/miden-sdk';

/**
 * Fee conversion info committed via a transaction's auth args.
 *
 * Since protocol 0.16 the auth args of a multisig transaction carry double duty.
 * `miden::standards::fee::load_conversion_info` requires them to be the
 * commitment `hash(CONVERSION_INFO || SALT)`, with the preimage supplied through
 * the advice map; the auth procedure then reuses that same word as the
 * transaction summary salt (see `guarded_multisig.masm`).
 *
 * A bare random word satisfies the salt role but not the fee role, and fails
 * late: the advice-map lookup misses, `load_conversion_info` returns the empty
 * word, and `pay_fee` aborts with `ERR_FEE_CONVERSION_INFO_MISSING` — but only
 * once the computed fee is non-zero. On a chain whose `verification_base_fee` is
 * zero the bare word is accepted, so the fault appears on the chain that
 * charges, not in development. The commitment is therefore unconditional:
 * `load_conversion_info` runs before the fee amount is known, and a valid
 * commitment verifies fine when the fee turns out to be zero.
 *
 * Interoperability survives that because the committed value is derivable rather
 * than arbitrary. The faucet is read from the block the proposal is anchored at,
 * which travels with the proposal, and the rate is fixed at 1/1 — so a rebuilder
 * holding `salt_hex` and the anchor reproduces the auth arg without being told
 * it. Both SDKs derive it the same way; see `docs/MIDEN_COMPATIBILITY.md`.
 */

/** Rate numerator/denominator for paying the fee in the native asset 1:1. */
const NATIVE_RATE_NUM = 1n;
const NATIVE_RATE_DEN = 1n;

/** Two words — the only length at which `hashElements` equals `merge`. */
const MERGE_EQUIVALENT_ELEMENTS = 8;

/**
 * Parses a caller-supplied faucet id, naming the option it came from — several
 * account ids are parsed while building a request, so the SDK's own message does
 * not say which one was rejected.
 */
function parseFeeFaucetId(feeFaucetId: string): AccountId {
  try {
    return AccountId.fromHex(feeFaucetId);
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    throw new Error(`Invalid feeFaucetId '${feeFaucetId}': ${detail}`, { cause: error });
  }
}

/**
 * Builds conversion info paying the fee at rate 1/1 out of the given faucet.
 *
 * This mirrors the word layout of `fee::native_conversion_info` and the rate of
 * `FeeConversionInfo::one_to_one`, but not the faucet: that MASM proc reads the
 * chain's own fee faucet from the reference block, whereas the faucet here is
 * whatever the caller passed and is not checked against any chain.
 *
 * Word layout is `[faucet_id_suffix, faucet_id_prefix, rate_num, rate_den]`.
 *
 * A hex argument is parsed into a handle owned here and released before
 * returning; an `AccountId` argument stays the caller's to free.
 */
export function nativeConversionInfo(feeFaucetId: AccountId | string): Word {
  if (typeof feeFaucetId !== 'string') {
    return conversionInfoWord(feeFaucetId);
  }

  const parsed = parseFeeFaucetId(feeFaucetId);
  try {
    return conversionInfoWord(parsed);
  } finally {
    parsed.free?.();
  }
}

function conversionInfoWord(accountId: AccountId): Word {
  return WordType.newFromFelts([
    accountId.suffix(),
    accountId.prefix(),
    new Felt(NATIVE_RATE_NUM),
    new Felt(NATIVE_RATE_DEN),
  ]);
}

/**
 * Computes the auth-arg commitment `hash(CONVERSION_INFO || SALT)`.
 *
 * The MASM computes this with `poseidon2::merge`, which the JS bindings do not
 * expose. `Poseidon2.hashElements` over the same eight elements is equivalent:
 * `merge` seeds a zero capacity, and `hash_elements` sets
 * `capacity[0] = len % RATE_WIDTH`, which for exactly eight elements is zero.
 * Both then absorb one full rate block and apply a single permutation, with no
 * padding block. Verified against `miden-crypto` — the digests are identical,
 * and the reversed argument order is not, so the ordering below matters.
 *
 * That equivalence holds only at exactly eight elements, which is why the length
 * is asserted rather than assumed: at any other length `hash_elements` seeds a
 * non-zero capacity or absorbs a padding block, and would silently stop agreeing
 * with the MASM.
 */
export function feeAuthArg(conversionInfo: Word, salt: Word): Word {
  const elements = [...conversionInfo.toFelts(), ...salt.toFelts()];
  if (elements.length !== MERGE_EQUIVALENT_ELEMENTS) {
    throw new Error(
      `Fee auth-arg preimage must be exactly ${MERGE_EQUIVALENT_ELEMENTS} elements to match ` +
        `poseidon2::merge, got ${elements.length}`,
    );
  }

  return Poseidon2.hashElements(new FeltArray(elements));
}

/**
 * Records the commitment preimage so `load_conversion_info` can recover it.
 *
 * Note the ordering asymmetry, which is easy to get wrong: the advice map value
 * is `SALT ++ CONVERSION_INFO` (the MASM pops the salt word first), while the
 * commitment hashes `CONVERSION_INFO ++ SALT`.
 */
export function insertFeeConversionInfo(
  adviceMap: AdviceMap,
  authArg: Word,
  conversionInfo: Word,
  salt: Word,
): void {
  adviceMap.insert(
    authArg,
    new FeltArray([...salt.toFelts(), ...conversionInfo.toFelts()]),
  );
}

/**
 * Resolves the auth arg a request should carry for the given salt.
 *
 * With a fee faucet the auth arg becomes the conversion-info commitment and the
 * returned advice map carries its preimage; both must reach the builder. Without
 * one the salt is used bare, which is the pre-0.16 behaviour and only valid on a
 * chain whose `verification_base_fee` is zero.
 *
 * The caller keeps the *inner* salt, never the returned auth arg: the commitment
 * is not invertible, and rebuilding the request at execution time re-derives the
 * commitment from that salt.
 *
 * On the bare path the returned auth arg *is* the salt handle, not a copy, so it
 * must not be freed separately. Every caller passes a salt it built for this one
 * call and does not reuse, which is what makes that safe; a caller that needs
 * its salt afterwards has to pass a copy.
 */
export function resolveAuthArg(
  salt: Word,
  feeFaucetId?: AccountId | string,
): { authArg: Word; adviceMap?: AdviceMap } {
  if (feeFaucetId === undefined) {
    return { authArg: salt };
  }

  const conversionInfo = nativeConversionInfo(feeFaucetId);
  let authArg: Word | undefined;
  let adviceMap: AdviceMap | undefined;
  try {
    authArg = feeAuthArg(conversionInfo, salt);
    adviceMap = new AdviceMap();
    insertFeeConversionInfo(adviceMap, authArg, conversionInfo, salt);

    return { authArg, adviceMap };
  } catch (error) {
    authArg?.free?.();
    adviceMap?.free?.();
    throw error;
  } finally {
    conversionInfo.free?.();
  }
}

/**
 * {@link resolveAuthArg}, extended to own the salt on the failing path.
 *
 * Resolution rejects an unparseable faucet id before it has taken ownership of
 * anything, so without this the salt {@link applyAuthArg} consumes would outlive
 * the call it was built for.
 */
function resolveOwnedAuthArg(
  salt: Word,
  feeFaucetId?: AccountId | string,
): { authArg: Word; adviceMap?: AdviceMap } {
  try {
    return resolveAuthArg(salt, feeFaucetId);
  } catch (error) {
    salt.free?.();
    throw error;
  }
}

/**
 * Applies the auth arg for `salt` to a builder, owning every handle involved.
 *
 * `withAuthArg` and `extendAdviceMap` borrow their arguments — the generated
 * glue passes `__wbg_ptr` without taking it — so the auth arg and the advice map
 * stay the caller's to release once the builder has read them. Doing that at
 * each call site meant five builders repeating a lifetime that only one of them
 * has to get wrong to leak, so it lives here instead.
 *
 * The salt is consumed, on the failing path too. On the bare path it *is* the
 * returned auth arg, so the two are freed once; on the committed path it is a
 * separate handle the commitment has already absorbed. Callers that need the
 * salt afterwards pass a copy — which every builder does, since it returns one.
 */
export function applyAuthArg(
  builder: TransactionRequestBuilder,
  salt: Word,
  feeFaucetId?: AccountId | string,
): TransactionRequestBuilder {
  const { authArg, adviceMap } = resolveOwnedAuthArg(salt, feeFaucetId);
  try {
    const withArg = builder.withAuthArg(authArg);
    return adviceMap === undefined ? withArg : withArg.extendAdviceMap(adviceMap);
  } finally {
    authArg.free?.();
    if (authArg !== salt) {
      salt.free?.();
    }
    adviceMap?.free?.();
  }
}

/** Which of the two auth-arg conventions produced a signed auth arg. */
export type AuthArgConvention = 'bare' | 'committed' | 'mismatch';

/**
 * Recovers which convention built an auth arg, by reproducing both.
 *
 * The inverse of {@link resolveAuthArg}, and the reason a rebuild can reproduce
 * a proposal it did not create: the commitment is not invertible, so the only
 * way to tell a bare salt from a commitment to that salt is to compute the
 * commitment and compare. `mismatch` means the auth arg belongs to neither —
 * a different salt, or a different fee faucet.
 *
 * Kept beside the producer so both can be checked against each other over the
 * real hash — see `tests/fee-auth-convention.test.ts`. That pairing catches a
 * detector that disagrees with the producer, but not a change to the hash they
 * share: swapping its operands moves both sides together and still reads as
 * `committed`. Operand order is pinned instead by the Rust-computed
 * `Poseidon2::merge` vector in `feeAuth.test.ts`.
 */
export function detectAuthArgConvention(
  signedAuthArg: Word,
  salt: Word,
  feeFaucetId: AccountId | string,
): AuthArgConvention {
  const signedHex = signedAuthArg.toHex();
  if (salt.toHex() === signedHex) {
    return 'bare';
  }

  const conversionInfo = nativeConversionInfo(feeFaucetId);
  try {
    const commitment = feeAuthArg(conversionInfo, salt);
    try {
      return commitment.toHex() === signedHex ? 'committed' : 'mismatch';
    } finally {
      commitment.free?.();
    }
  } finally {
    conversionInfo.free?.();
  }
}
