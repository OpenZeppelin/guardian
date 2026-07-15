import { AdviceMap, Felt, FeltArray, Poseidon2, Word } from '@miden-sdk/miden-sdk';
import { describe, expect, it } from 'vitest';

import { buildMultisigConfigAdvice } from '../src/transaction/updateSigners.js';

const SIGNER_COMMITMENT =
  '0x260a375ca01f1f05cd7bf22298b40c47290fc09f209011d39049b7f2ef61387b';

describe('update-signers advice FeltArray ownership', () => {
  it('Poseidon2.hashElements consumes its FeltArray (the bug precondition)', () => {
    const felts = [new Felt(1n), new Felt(1n), new Felt(0n), new Felt(0n)];
    const reused = new FeltArray(felts);

    const key = Poseidon2.hashElements(reused);

    const advice = new AdviceMap();
    expect(() => advice.insert(key, reused)).toThrow();
  });

  it('buildMultisigConfigAdvice returns a payload that survives advice.insert', () => {
    const { configHash, payload } = buildMultisigConfigAdvice(1, [SIGNER_COMMITMENT], 'falcon');

    const advice = new AdviceMap();
    expect(() => advice.insert(configHash, payload)).not.toThrow();
  });
});
