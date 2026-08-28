import { AdviceMap, FeltArray, TransactionRequestBuilder, Word } from '@miden-sdk/miden-sdk';
import { describe, expect, it } from 'vitest';

import { nativeConversionInfo, resolveAuthArg } from '../src/transaction/feeAuth.js';

/**
 * Carries the fee wiring through a real `TransactionRequest` and reads it back
 * out of the built object, which is as close to the VM as this package gets
 * without a node. The builder tests in `src/transaction/feeWiring.test.ts`
 * record the calls against a fake builder, so they prove the arguments the
 * builders pass and nothing about whether the SDK stores them: an auth arg the
 * WASM silently dropped, or an advice entry keyed off something other than the
 * commitment, passes there and aborts in `pay_fee`.
 *
 * The remaining untested link is the VM itself. Nothing here executes MASM, so
 * an operand order that disagrees with `load_conversion_info` still round trips
 * — see the note in `fee-auth-convention.test.ts`.
 */
const FEE_FAUCET = '0xade67f7701e9e9c12493c6206bc46e';
const SALT = Word.fromHex('0x' + '11'.repeat(32));

/**
 * The map exposes no reader, only `insert`, which returns the value it
 * displaced. That leaves the entry replaced, so this consumes the map.
 */
function takeEntry(adviceMap: AdviceMap, key: Word): bigint[] | undefined {
  return adviceMap.insert(key, new FeltArray([]))?.map((felt) => felt.asInt());
}

function feltInts(word: Word): bigint[] {
  return word.toFelts().map((felt) => felt.asInt());
}

describe('fee auth through a real transaction request', () => {
  it('stores the commitment as the auth arg and its preimage in the advice map', () => {
    const { authArg, adviceMap } = resolveAuthArg(SALT, FEE_FAUCET);
    const commitment = authArg.toHex();
    expect(adviceMap).toBeDefined();

    const request = new TransactionRequestBuilder()
      .withAuthArg(authArg)
      .extendAdviceMap(adviceMap as AdviceMap)
      .build();

    expect(request.authArg()?.toHex()).toBe(commitment);
    expect(takeEntry(request.adviceMap(), Word.fromHex(commitment))).toEqual([
      ...feltInts(SALT),
      ...feltInts(nativeConversionInfo(FEE_FAUCET)),
    ]);
  });

  it('stores the bare salt with no advice entry when no faucet is given', () => {
    const { authArg, adviceMap } = resolveAuthArg(SALT);
    expect(adviceMap).toBeUndefined();

    const request = new TransactionRequestBuilder().withAuthArg(authArg).build();

    expect(request.authArg()?.toHex()).toBe(SALT.toHex());
    expect(takeEntry(request.adviceMap(), SALT)).toBeUndefined();
  });
});
