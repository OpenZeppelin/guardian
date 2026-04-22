import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

import {
  MULTISIG_ECDSA_MASM,
  MULTISIG_MASM,
  GUARDIAN_ECDSA_MASM,
  GUARDIAN_MASM,
} from '../src/account/masm/auth.js';
import {
  MULTISIG_ACCOUNT_COMPONENT_MASM,
  MULTISIG_ECDSA_ACCOUNT_COMPONENT_MASM,
  MULTISIG_GUARDIAN_ACCOUNT_COMPONENT_MASM,
  MULTISIG_GUARDIAN_ECDSA_ACCOUNT_COMPONENT_MASM,
} from '../src/account/masm/account-components/auth.js';

describe('generated MASM constants', () => {
  it('matches the packaged multisig MASM source', () => {
    const expected = readFileSync(new URL('../masm/auth/multisig.masm', import.meta.url), 'utf8');
    expect(MULTISIG_MASM).toBe(expected);
  });

  it('matches the packaged GUARDIAN MASM source', () => {
    const expected = readFileSync(new URL('../masm/auth/guardian.masm', import.meta.url), 'utf8');
    expect(GUARDIAN_MASM).toBe(expected);
  });

  it('matches the packaged ECDSA multisig MASM source', () => {
    const expected = readFileSync(new URL('../masm/auth/multisig_ecdsa.masm', import.meta.url), 'utf8');
    expect(MULTISIG_ECDSA_MASM).toBe(expected);
  });

  it('matches the packaged ECDSA GUARDIAN MASM source', () => {
    const expected = readFileSync(new URL('../masm/auth/guardian_ecdsa.masm', import.meta.url), 'utf8');
    expect(GUARDIAN_ECDSA_MASM).toBe(expected);
  });

  it('matches the packaged multisig account-component MASM source', () => {
    const expected = readFileSync(
      new URL('../masm/account_components/auth/multisig.masm', import.meta.url),
      'utf8',
    );
    expect(MULTISIG_ACCOUNT_COMPONENT_MASM).toBe(expected);
  });

  it('matches the packaged multisig+GUARDIAN account-component MASM source', () => {
    const expected = readFileSync(
      new URL('../masm/account_components/auth/multisig_guardian.masm', import.meta.url),
      'utf8',
    );
    expect(MULTISIG_GUARDIAN_ACCOUNT_COMPONENT_MASM).toBe(expected);
  });

  it('matches the packaged ECDSA multisig account-component MASM source', () => {
    const expected = readFileSync(
      new URL('../masm/account_components/auth/multisig_ecdsa.masm', import.meta.url),
      'utf8',
    );
    expect(MULTISIG_ECDSA_ACCOUNT_COMPONENT_MASM).toBe(expected);
  });

  it('matches the packaged ECDSA multisig+GUARDIAN account-component MASM source', () => {
    const expected = readFileSync(
      new URL('../masm/account_components/auth/multisig_guardian_ecdsa.masm', import.meta.url),
      'utf8',
    );
    expect(MULTISIG_GUARDIAN_ECDSA_ACCOUNT_COMPONENT_MASM).toBe(expected);
  });
});
