import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createMultisigAccount, validateMultisigConfig } from './builder.js';
import { GUARDED_MULTISIG_ACCOUNT_COMPONENT_MASM } from './masm/account-components/auth.js';

const {
  buildMultisigStorageSlots,
  buildGuardianStorageSlots,
  withSupportsAllTypes,
  compileComponent,
  MockAccountBuilder,
} = vi.hoisted(() => {
  const buildMultisigStorageSlots = vi.fn(() => ['multisig-slots']);
  const buildGuardianStorageSlots = vi.fn(() => ['guardian-slots']);
  const withSupportsAllTypes = vi.fn((component) => component);
  const compileComponent = vi.fn((code, slots) => ({
    code,
    slots,
    withSupportsAllTypes: () => withSupportsAllTypes({ code, slots }),
  }));

  class MockAccountBuilder {
    accountType() {
      return this;
    }

    storageMode() {
      return this;
    }

    withAuthComponent() {
      return this;
    }

    withComponent() {
      return this;
    }

    withBasicWalletComponent() {
      return this;
    }

    build() {
      return {
        account: { id: () => ({ toString: () => '0x' + 'a'.repeat(30) }) },
      };
    }

    buildWithoutSchemaCommitment() {
      return {
        account: { id: () => ({ toString: () => '0x' + 'a'.repeat(30) }) },
      };
    }
  }

  return {
    buildMultisigStorageSlots,
    buildGuardianStorageSlots,
    withSupportsAllTypes,
    compileComponent,
    MockAccountBuilder,
  };
});

vi.mock('./storage.js', () => ({
  buildMultisigStorageSlots,
  buildGuardianStorageSlots,
}));

vi.mock('@miden-sdk/miden-sdk', () => ({
  AccountBuilder: MockAccountBuilder,
  AccountComponent: {
    compile: compileComponent,
  },
  AccountStorageMode: {
    public: () => 'public',
    private: () => 'private',
  },
}));

describe('createMultisigAccount', () => {
  beforeEach(() => {
    vi.stubGlobal('crypto', {
      getRandomValues(buffer: Uint8Array) {
        return buffer;
      },
    });
    buildMultisigStorageSlots.mockClear();
    buildGuardianStorageSlots.mockClear();
    withSupportsAllTypes.mockClear();
    compileComponent.mockClear();
  });

  function makeClient() {
    const authBuilder = {
      linkModule: vi.fn(),
      compileAccountComponentCode: vi.fn((source) => ({ source })),
    };
    const webClient = {
      createCodeBuilder: vi.fn().mockReturnValue(authBuilder),
      accounts: {
        insert: vi.fn().mockResolvedValue(undefined),
      },
    };
    return { authBuilder, webClient };
  }

  it('compiles the guarded component without re-linking SDK-provided modules (Falcon)', async () => {
    const { authBuilder, webClient } = makeClient();

    await createMultisigAccount(
      webClient as never,
      {
        threshold: 1,
        signerCommitments: ['0x' + '1'.repeat(64)],
        guardianCommitment: '0x' + '2'.repeat(64),
      },
      'http://localhost:57291',
    );

    // The web SDK assembler already provides `miden::standards::auth::*`; re-linking would
    // raise a duplicate-definition error, so the builder must NOT call linkModule.
    expect(authBuilder.linkModule).not.toHaveBeenCalled();
    expect(authBuilder.compileAccountComponentCode).toHaveBeenCalledWith(
      GUARDED_MULTISIG_ACCOUNT_COMPONENT_MASM,
    );
    expect(webClient.accounts.insert).toHaveBeenCalledTimes(1);
  });

  it('uses the same scheme-agnostic component for ECDSA', async () => {
    const { authBuilder, webClient } = makeClient();

    await createMultisigAccount(
      webClient as never,
      {
        threshold: 1,
        signerCommitments: ['0x' + '1'.repeat(64)],
        guardianCommitment: '0x' + '2'.repeat(64),
        signatureScheme: 'ecdsa',
      },
      'http://localhost:57291',
    );

    expect(authBuilder.linkModule).not.toHaveBeenCalled();
    expect(authBuilder.compileAccountComponentCode).toHaveBeenCalledWith(
      GUARDED_MULTISIG_ACCOUNT_COMPONENT_MASM,
    );
    expect(webClient.accounts.insert).toHaveBeenCalledTimes(1);
  });
});

describe('validateMultisigConfig', () => {
  const signer = '0x' + '1'.repeat(64);

  it('rejects a guardian commitment equal to a signer (matches upstream Rust invariant)', () => {
    expect(() =>
      validateMultisigConfig({
        threshold: 1,
        signerCommitments: [signer],
        guardianCommitment: signer,
      }),
    ).toThrow(/different from all signer commitments/);
  });

  it('accepts a distinct guardian commitment', () => {
    expect(() =>
      validateMultisigConfig({
        threshold: 1,
        signerCommitments: [signer],
        guardianCommitment: '0x' + '2'.repeat(64),
      }),
    ).not.toThrow();
  });
});
