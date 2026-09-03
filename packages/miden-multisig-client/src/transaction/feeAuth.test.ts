import type { TransactionRequestBuilder } from '@miden-sdk/miden-sdk';
import { AccountId, Felt, FeltArray, Word } from '@miden-sdk/miden-sdk';
import { describe, expect, it, vi } from 'vitest';

import { applyAuthArg, feeAuthArg, nativeConversionInfo, resolveAuthArg } from './feeAuth.js';

// The account id the Rust cross-SDK parity test pins.
const FEE_FAUCET = '0xade67f7701e9e9c12493c6206bc46e';

const word = (...values: bigint[]): Word =>
  Word.newFromFelts(values.map((value) => new Felt(value)));

const felts = (value: Word): bigint[] => value.toFelts().map((felt) => felt.asInt());

describe('feeAuthArg', () => {
  /**
   * The MASM commits with `poseidon2::merge`, which the JS bindings do not
   * expose, so this uses `Poseidon2.hashElements` over the same eight elements.
   * The two agree only because eight elements fill exactly one rate block and
   * leave the capacity zero — an equivalence that would break silently if the
   * preimage ever stopped being 4+4 elements.
   *
   * This vector is the output of `Poseidon2::merge(&[CONV, SALT])` computed
   * directly against `miden-crypto` 0.29.1, so it pins the JS path to the Rust
   * one rather than to itself.
   */
  it('matches Poseidon2::merge computed in Rust', () => {
    const conversionInfo = word(0xdeadn, 0xbeefn, 1n, 1n);
    const salt = word(11n, 22n, 33n, 44n);

    expect(felts(feeAuthArg(conversionInfo, salt))).toEqual([
      8229881116367716759n,
      4629940889584181757n,
      3690706593687614873n,
      14641540369284480384n,
    ]);
  });
});

describe('nativeConversionInfo', () => {
  it('lays out [suffix, prefix, 1, 1] to match fee::native_conversion_info', () => {
    const faucet = AccountId.fromHex(FEE_FAUCET);

    expect(felts(nativeConversionInfo(FEE_FAUCET))).toEqual([
      faucet.suffix().asInt(),
      faucet.prefix().asInt(),
      1n,
      1n,
    ]);
  });

  /**
   * Several account ids are parsed while building a request, and the SDK's own
   * decode error names none of them. Without the option's name in the message a
   * caller cannot tell which field they got wrong.
   */
  it('names the option when its faucet id will not parse', () => {
    expect(() => nativeConversionInfo('0xnotanaccount')).toThrow(
      /Invalid feeFaucetId '0xnotanaccount': /,
    );
  });
});

describe('resolveAuthArg', () => {
  const faucet = FEE_FAUCET;

  it('commits to the conversion info when a fee faucet is given', () => {
    const salt = word(11n, 22n, 33n, 44n);
    const { authArg, adviceMap } = resolveAuthArg(salt, faucet);

    expect(adviceMap).toBeDefined();
    expect(felts(authArg)).toEqual(
      felts(feeAuthArg(nativeConversionInfo(faucet), salt)),
    );
    // The commitment must not be the salt: passing the salt through bare is
    // exactly the pre-0.16 bug this replaces.
    expect(felts(authArg)).not.toEqual(felts(salt));
  });

  it('stores the preimage as SALT ++ CONVERSION_INFO under the commitment', () => {
    // The advice value is ordered opposite to the hashed preimage, because
    // `load_conversion_info` pops the salt word first. Nothing else in the suite
    // would catch the two being swapped, and on-chain it would surface only as a
    // commitment mismatch at execution.
    const salt = word(11n, 22n, 33n, 44n);
    const { authArg, adviceMap } = resolveAuthArg(salt, faucet);

    // `insert` returns the previous value, which is the only read path the
    // binding exposes.
    const stored = adviceMap!.insert(authArg, new FeltArray([]));

    expect(stored?.map((felt) => felt.asInt())).toEqual([
      ...felts(salt),
      ...felts(nativeConversionInfo(faucet)),
    ]);
  });

  it('falls back to the bare salt without a fee faucet', () => {
    const salt = word(11n, 22n, 33n, 44n);
    const { authArg, adviceMap } = resolveAuthArg(salt);

    expect(felts(authArg)).toEqual(felts(salt));
    expect(adviceMap).toBeUndefined();
  });

  /**
   * The bare path hands back the salt handle itself rather than a copy, so a
   * caller must not free the auth arg separately. Pinned because the alternative
   * — allocating a copy — buys nothing: every caller already passes a salt built
   * for that one call and never frees it, so the copy would just be a second
   * handle on the default path waiting on the finalization registry.
   */
  it('aliases the salt on the bare path rather than allocating a copy', () => {
    const salt = word(11n, 22n, 33n, 44n);
    const { authArg } = resolveAuthArg(salt);

    expect(authArg).toBe(salt);
  });

  /**
   * `applyAuthArg` documents the salt as consumed, and resolution rejects an
   * unparseable faucet id before it has taken ownership of anything — so the
   * throw escapes before the cleanup scope that would otherwise release it.
   */
  it('frees the salt when resolution rejects the fee faucet', () => {
    const salt = word(11n, 22n, 33n, 44n);
    const free = vi.spyOn(salt, 'free');
    const builder = {
      withAuthArg: () => builder,
      extendAdviceMap: () => builder,
    } as unknown as TransactionRequestBuilder;

    expect(() => applyAuthArg(builder, salt, 'not-a-faucet-id')).toThrow();
    expect(free).toHaveBeenCalledTimes(1);
  });
});
