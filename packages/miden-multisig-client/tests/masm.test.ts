import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

import {
  MULTISIG_ECDSA_MASM,
  MULTISIG_MASM,
  PSM_ECDSA_MASM,
  PSM_MASM,
} from '../src/account/masm/auth.js';
import {
  MULTISIG_ACCOUNT_COMPONENT_MASM,
  MULTISIG_ECDSA_ACCOUNT_COMPONENT_MASM,
  MULTISIG_PSM_ACCOUNT_COMPONENT_MASM,
  MULTISIG_PSM_ECDSA_ACCOUNT_COMPONENT_MASM,
} from '../src/account/masm/account-components/auth.js';

describe('generated MASM constants', () => {
  it('matches the packaged multisig MASM source', () => {
    const expected = readFileSync(new URL('../masm/auth/multisig.masm', import.meta.url), 'utf8');
    expect(MULTISIG_MASM).toBe(expected);
  });

  it('matches the packaged PSM MASM source', () => {
    const expected = readFileSync(new URL('../masm/auth/psm.masm', import.meta.url), 'utf8');
    expect(PSM_MASM).toBe(expected);
  });

  it('matches the packaged ECDSA multisig MASM source', () => {
    const expected = readFileSync(new URL('../masm/auth/multisig_ecdsa.masm', import.meta.url), 'utf8');
    expect(MULTISIG_ECDSA_MASM).toBe(expected);
  });

  it('matches the packaged ECDSA PSM MASM source', () => {
    const expected = readFileSync(new URL('../masm/auth/psm_ecdsa.masm', import.meta.url), 'utf8');
    expect(PSM_ECDSA_MASM).toBe(expected);
  });

  it('matches the packaged multisig account-component MASM source', () => {
    const expected = readFileSync(
      new URL('../masm/account_components/auth/multisig.masm', import.meta.url),
      'utf8',
    );
    expect(MULTISIG_ACCOUNT_COMPONENT_MASM).toBe(expected);
  });

  it('matches the packaged multisig+PSM account-component MASM source', () => {
    const expected = readFileSync(
      new URL('../masm/account_components/auth/multisig_psm.masm', import.meta.url),
      'utf8',
    );
    expect(MULTISIG_PSM_ACCOUNT_COMPONENT_MASM).toBe(expected);
  });

  it('matches the packaged ECDSA multisig account-component MASM source', () => {
    const expected = readFileSync(
      new URL('../masm/account_components/auth/multisig_ecdsa.masm', import.meta.url),
      'utf8',
    );
    expect(MULTISIG_ECDSA_ACCOUNT_COMPONENT_MASM).toBe(expected);
  });

  it('matches the packaged ECDSA multisig+PSM account-component MASM source', () => {
    const expected = readFileSync(
      new URL('../masm/account_components/auth/multisig_psm_ecdsa.masm', import.meta.url),
      'utf8',
    );
    expect(MULTISIG_PSM_ECDSA_ACCOUNT_COMPONENT_MASM).toBe(expected);
  });
});
