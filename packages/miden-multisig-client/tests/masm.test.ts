import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

import { MULTISIG_MASM, PSM_MASM } from '../src/account/masm.js';

describe('generated MASM constants', () => {
  it('matches the packaged multisig MASM source', () => {
    const expected = readFileSync(new URL('../masm/multisig.masm', import.meta.url), 'utf8');
    expect(MULTISIG_MASM).toBe(expected);
  });

  it('matches the packaged PSM MASM source', () => {
    const expected = readFileSync(new URL('../masm/psm.masm', import.meta.url), 'utf8');
    expect(PSM_MASM).toBe(expected);
  });
});
