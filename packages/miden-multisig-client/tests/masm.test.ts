import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

import { GUARDED_MULTISIG_ACCOUNT_COMPONENT_MASM } from '../src/account/masm/account-components/auth.js';

// Only the guarded-multisig account-component shell is vendored and compiled by the TS builder;
// its `miden::standards::auth::*` library dependencies are provided by the web SDK assembler.
describe('generated MASM constants', () => {
  it('matches the vendored guarded-multisig account-component source', () => {
    const expected = readFileSync(
      new URL('../masm/account_components/auth/guarded_multisig.masm', import.meta.url),
      'utf8',
    );
    expect(GUARDED_MULTISIG_ACCOUNT_COMPONENT_MASM).toBe(expected);
  });
});
