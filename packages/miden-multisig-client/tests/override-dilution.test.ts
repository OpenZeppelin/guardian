import { describe, expect, it } from 'vitest';

import { Multisig } from '../src/multisig.js';
import type { ProcedureName } from '../src/procedures.js';

type DilutionReceiver = Pick<Multisig, 'signerCommitments' | 'procedureThresholds'>;

function receiver(
  numSigners: number,
  overrides: Array<[ProcedureName, number]>,
): DilutionReceiver {
  return {
    signerCommitments: Array.from({ length: numSigners }, (_, i) => `0x${i + 1}`),
    procedureThresholds: new Map(overrides),
  };
}

function diluted(target: DilutionReceiver, newNumSigners: number) {
  return Multisig.prototype.overridesDilutedBySignerGrowth.call(target, newNumSigners);
}

describe('overridesDilutedBySignerGrowth', () => {
  const overrides: Array<[ProcedureName, number]> = [
    ['send_asset', 1],
    ['update_signers', 3],
  ];

  it('lists every configured override when the signer set grows', () => {
    expect(diluted(receiver(3, overrides), 4)).toEqual([
      { procedure: 'send_asset', threshold: 1 },
      { procedure: 'update_signers', threshold: 3 },
    ]);
  });

  it('reports nothing when the signer count is unchanged or shrinks', () => {
    expect(diluted(receiver(3, overrides), 3)).toEqual([]);
    expect(diluted(receiver(3, overrides), 2)).toEqual([]);
  });

  it('reports nothing when no overrides are configured', () => {
    expect(diluted(receiver(3, []), 4)).toEqual([]);
  });
});
