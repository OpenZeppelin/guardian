import { Word } from '@miden-sdk/miden-sdk';
import { describe, expect, it } from 'vitest';

import { buildMultisigConfigAdvice } from './updateSigners.js';

const A = '0x' + '01'.repeat(32);
const B = '0x' + '02'.repeat(32);
const C = '0x' + '03'.repeat(32);

function payloadFelts(payload: { length(): number; get(index: number): { asInt(): bigint } }) {
  return Array.from({ length: payload.length() }, (_, i) => payload.get(i).asInt());
}

function wordFelts(hex: string): bigint[] {
  return Word.fromHex(hex).toFelts().map((felt) => felt.asInt());
}

describe('buildMultisigConfigAdvice', () => {
  it('interleaves each approver with its own scheme id, last approver first', () => {
    const { payload } = buildMultisigConfigAdvice(
      2,
      [A, { commitment: B, scheme: 'ecdsa' }, { commitment: C, scheme: 'falcon' }],
      'ecdsa',
    );

    expect(payloadFelts(payload)).toEqual([
      2n, 3n, 0n, 0n,
      ...wordFelts(C), 2n, 0n, 0n, 0n,
      ...wordFelts(B), 1n, 0n, 0n, 0n,
      ...wordFelts(A), 1n, 0n, 0n, 0n,
    ]);
  });

  it('hashes the same config for bare commitments and equivalent specs', () => {
    const bare = buildMultisigConfigAdvice(1, [A, B], 'falcon');
    const specs = buildMultisigConfigAdvice(
      1,
      [
        { commitment: A, scheme: 'falcon' },
        { commitment: B, scheme: 'falcon' },
      ],
      'ecdsa',
    );

    expect(specs.configHash.toHex()).toBe(bare.configHash.toHex());
  });

  it('hashes a different config when one approver changes scheme', () => {
    const uniform = buildMultisigConfigAdvice(1, [A, B], 'falcon');
    const mixed = buildMultisigConfigAdvice(1, [A, { commitment: B, scheme: 'ecdsa' }], 'falcon');

    expect(mixed.configHash.toHex()).not.toBe(uniform.configHash.toHex());
  });
});
