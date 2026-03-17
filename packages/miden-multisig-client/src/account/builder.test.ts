import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createMultisigAccount } from './builder.js';
import {
  MULTISIG_ECDSA_MASM,
  MULTISIG_MASM,
  PSM_ECDSA_MASM,
  PSM_MASM,
} from './masm.js';

const {
  buildMultisigStorageSlots,
  buildPsmStorageSlots,
  withSupportsAllTypes,
  compileComponent,
  MockAccountBuilder,
} = vi.hoisted(() => {
  const buildMultisigStorageSlots = vi.fn(() => ['multisig-slots']);
  const buildPsmStorageSlots = vi.fn(() => ['psm-slots']);
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
    buildPsmStorageSlots,
    withSupportsAllTypes,
    compileComponent,
    MockAccountBuilder,
  };
});

vi.mock('./storage.js', () => ({
  buildMultisigStorageSlots,
  buildPsmStorageSlots,
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
    buildPsmStorageSlots.mockClear();
    withSupportsAllTypes.mockClear();
    compileComponent.mockClear();
  });

  it('uses Falcon MASM by default', async () => {
    const psmBuilder = {
      compileAccountComponentCode: vi.fn((source) => `psm:${source.slice(0, 16)}`),
    };
    const multisigBuilder = {
      buildLibrary: vi.fn((libraryPath, source) => ({ libraryPath, source })),
      linkStaticLibrary: vi.fn(),
      compileAccountComponentCode: vi.fn((source) => `multisig:${source.slice(0, 16)}`),
    };
    const webClient = {
      createCodeBuilder: vi
        .fn()
        .mockReturnValueOnce(psmBuilder)
        .mockReturnValueOnce(multisigBuilder),
      newAccount: vi.fn(),
    };

    await createMultisigAccount(webClient as never, {
      threshold: 1,
      signerCommitments: ['0x' + '1'.repeat(64)],
      psmCommitment: '0x' + '2'.repeat(64),
    });

    expect(psmBuilder.compileAccountComponentCode).toHaveBeenCalledWith(PSM_MASM);
    expect(multisigBuilder.buildLibrary).toHaveBeenCalledWith('openzeppelin::psm', PSM_MASM);
    expect(multisigBuilder.compileAccountComponentCode).toHaveBeenCalledWith(MULTISIG_MASM);
    expect(webClient.newAccount).toHaveBeenCalledTimes(1);
  });

  it('uses ECDSA MASM when requested', async () => {
    const psmBuilder = {
      compileAccountComponentCode: vi.fn((source) => `psm:${source.slice(0, 16)}`),
    };
    const multisigBuilder = {
      buildLibrary: vi.fn((libraryPath, source) => ({ libraryPath, source })),
      linkStaticLibrary: vi.fn(),
      compileAccountComponentCode: vi.fn((source) => `multisig:${source.slice(0, 16)}`),
    };
    const webClient = {
      createCodeBuilder: vi
        .fn()
        .mockReturnValueOnce(psmBuilder)
        .mockReturnValueOnce(multisigBuilder),
      newAccount: vi.fn(),
    };

    await createMultisigAccount(webClient as never, {
      threshold: 1,
      signerCommitments: ['0x' + '1'.repeat(64)],
      psmCommitment: '0x' + '2'.repeat(64),
      signatureScheme: 'ecdsa',
    });

    expect(psmBuilder.compileAccountComponentCode).toHaveBeenCalledWith(PSM_ECDSA_MASM);
    expect(multisigBuilder.buildLibrary).toHaveBeenCalledWith(
      'openzeppelin::psm_ecdsa',
      PSM_ECDSA_MASM,
    );
    expect(multisigBuilder.compileAccountComponentCode).toHaveBeenCalledWith(MULTISIG_ECDSA_MASM);
    expect(webClient.newAccount).toHaveBeenCalledTimes(1);
  });
});
