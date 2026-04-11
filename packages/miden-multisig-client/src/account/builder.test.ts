import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createMultisigAccount } from './builder.js';
import {
  MULTISIG_ECDSA_MASM,
  MULTISIG_MASM,
  GUARDIAN_ECDSA_MASM,
  GUARDIAN_MASM,
} from './masm/auth.js';
import {
  MULTISIG_GUARDIAN_ACCOUNT_COMPONENT_MASM,
  MULTISIG_GUARDIAN_ECDSA_ACCOUNT_COMPONENT_MASM,
} from './masm/account-components/auth.js';

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
  AccountType: {
    RegularAccountUpdatableCode: 'regular',
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

  it('uses Falcon MASM by default', async () => {
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

    await createMultisigAccount(webClient as never, {
      threshold: 1,
      signerCommitments: ['0x' + '1'.repeat(64)],
      guardianCommitment: '0x' + '2'.repeat(64),
    });

    expect(authBuilder.linkModule).toHaveBeenNthCalledWith(
      1,
      'openzeppelin::auth::guardian',
      GUARDIAN_MASM,
    );
    expect(authBuilder.linkModule).toHaveBeenNthCalledWith(
      2,
      'openzeppelin::auth::multisig',
      MULTISIG_MASM,
    );
    expect(authBuilder.compileAccountComponentCode).toHaveBeenCalledWith(
      MULTISIG_GUARDIAN_ACCOUNT_COMPONENT_MASM,
    );
    expect(webClient.accounts.insert).toHaveBeenCalledTimes(1);
  });

  it('uses ECDSA MASM when requested', async () => {
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

    await createMultisigAccount(webClient as never, {
      threshold: 1,
      signerCommitments: ['0x' + '1'.repeat(64)],
      guardianCommitment: '0x' + '2'.repeat(64),
      signatureScheme: 'ecdsa',
    });

    expect(authBuilder.linkModule).toHaveBeenNthCalledWith(
      1,
      'openzeppelin::auth::guardian_ecdsa',
      GUARDIAN_ECDSA_MASM,
    );
    expect(authBuilder.linkModule).toHaveBeenNthCalledWith(
      2,
      'openzeppelin::auth::multisig_ecdsa',
      MULTISIG_ECDSA_MASM,
    );
    expect(authBuilder.compileAccountComponentCode).toHaveBeenCalledWith(
      MULTISIG_GUARDIAN_ECDSA_ACCOUNT_COMPONENT_MASM,
    );
    expect(webClient.accounts.insert).toHaveBeenCalledTimes(1);
  });
});
