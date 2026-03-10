import { describe, expect, it, vi } from 'vitest';

vi.mock('@miden-sdk/miden-sdk', () => ({
  AccountBuilder: vi.fn(),
  AccountComponent: {
    compile: vi.fn(),
  },
  AccountType: {
    RegularAccountUpdatableCode: 'RegularAccountUpdatableCode',
  },
  AccountStorageMode: {
    public: vi.fn(),
    private: vi.fn(),
  },
}));

import { validateMultisigConfig } from './builder.js';

describe('validateMultisigConfig', () => {
  it('rejects duplicate signer commitments after normalization', () => {
    expect(() =>
      validateMultisigConfig({
        threshold: 1,
        signerCommitments: [
          '0x' + 'a'.repeat(64),
          '0x' + 'A'.repeat(64),
        ],
        psmCommitment: '0x' + 'b'.repeat(64),
      }),
    ).toThrow('duplicate signer commitment');
  });

  it('accepts distinct signer commitments', () => {
    expect(() =>
      validateMultisigConfig({
        threshold: 2,
        signerCommitments: [
          '0x' + 'a'.repeat(64),
          '0x' + 'b'.repeat(64),
        ],
        psmCommitment: '0x' + 'c'.repeat(64),
      }),
    ).not.toThrow();
  });
});
