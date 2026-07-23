import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { Word } from '@miden-sdk/miden-sdk';

const {
  mockFromVaultKey,
  mockFungibleAssetConstructor,
  mockHashElements,
  mockNormalizeHexWord,
  mockRandomWord,
  mockWordFromHex,
  saltFelts,
} = vi.hoisted(() => {
  const saltFelts = [
    { id: 'felt-0' },
    { id: 'felt-1' },
    { id: 'felt-2' },
    { id: 'felt-3' },
  ];

  return {
    mockFromVaultKey: vi.fn((vaultKey: unknown, amount: bigint) => ({
      kind: 'asset-from-vault-key',
      vaultKey,
      amount,
    })),
    mockFungibleAssetConstructor: vi.fn(),
    mockHashElements: vi.fn().mockReturnValue({ toString: () => 'serial' }),
    mockNormalizeHexWord: vi.fn((hex: string) => hex),
    mockRandomWord: vi.fn().mockReturnValue({
      toHex: () => '0x' + 'aa'.repeat(32),
    }),
    mockWordFromHex: vi.fn((hex: string) => {
      const normalized = hex.toLowerCase();
      return {
        toHex: () => hex,
        toFelts: () => normalized === `0x${'00'.repeat(32)}`
          ? [
              { value: 0n },
              { value: 0n },
              { value: 0n },
              { value: 0n },
            ]
          : saltFelts,
      };
    }),
    saltFelts,
  };
});

vi.mock('@miden-sdk/miden-sdk', () => {
  class Felt {
    readonly value: bigint;

    constructor(value: bigint) {
      this.value = value;
    }
  }

  class FeltArray {
    readonly values: unknown[];

    constructor(values: unknown[]) {
      this.values = values;
    }
  }

  class NoteAssets {
    constructor(_assets: unknown[]) {}
  }

  class NoteStorage {
    constructor(_inputs: FeltArray) {}
  }

  class NoteMetadata {
    constructor(
      _sender: unknown,
      _noteType: unknown,
      _noteTag: unknown,
    ) {}
  }

  class NoteRecipient {
    constructor(
      _serialNum: unknown,
      _noteScript: unknown,
      _noteInputs: unknown,
    ) {}
  }

  class Note {
    constructor(
      _assets: unknown,
      _metadata: unknown,
      _recipient: unknown,
    ) {}
  }

  class FungibleAsset {
    constructor(faucet: unknown, amount: bigint) {
      mockFungibleAssetConstructor(faucet, amount);
    }

    static fromVaultKey = mockFromVaultKey;
  }

  class NoteArray {
    constructor(_notes: unknown[]) {}
  }

  class TransactionRequestBuilder {
    withOwnOutputNotes(_notes: unknown): this {
      return this;
    }

    withAuthArg(_authArg: unknown): this {
      return this;
    }

    extendAdviceMap(_adviceMap: unknown): this {
      return this;
    }

    build(): { kind: 'request' } {
      return { kind: 'request' };
    }
  }

  return {
    AccountId: {
      fromHex: vi.fn((hex: string) => ({
        hex,
        prefix: () => 1,
        suffix: () => 2,
        toString: () => hex,
      })),
    },
    Felt,
    FeltArray,
    FungibleAsset,
    MidenArrays: {
      NoteArray,
    },
    Note,
    NoteAssets,
    NoteMetadata,
    NoteRecipient,
    NoteStorage,
    NoteScript: {
      p2id: vi.fn(() => ({ kind: 'p2id-script' })),
    },
    NoteTag: {
      withAccountTarget: vi.fn(() => ({ kind: 'tag' })),
    },
    NoteType: {
      Public: 'public',
    },
    OutputNote: {
      full: vi.fn((note: unknown) => ({ note })),
    },
    Poseidon2: {
      hashElements: mockHashElements,
    },
    TransactionRequestBuilder,
    Word: {
      fromHex: mockWordFromHex,
    },
  };
});

vi.mock('../utils/encoding.js', () => ({
  normalizeHexWord: mockNormalizeHexWord,
}));

vi.mock('../utils/random.js', () => ({
  randomWord: mockRandomWord,
}));

import { buildP2idTransactionRequest } from './p2id.js';
import type { Account } from '@miden-sdk/miden-sdk';

const FAUCET_ID = '0x7bfb0f38b0fafa103f86a805594171';

const mockAccount = {
  vault: () => ({
    fungibleAssets: () => [
      {
        faucetId: () => ({ toString: () => FAUCET_ID }),
        vaultKey: () => ({ kind: 'vault-key' }),
      },
    ],
  }),
} as unknown as Account;

describe('buildP2idTransactionRequest', () => {
  beforeEach(() => {
    mockFromVaultKey.mockClear();
    mockFungibleAssetConstructor.mockClear();
    mockHashElements.mockClear();
    mockNormalizeHexWord.mockClear();
    mockRandomWord.mockClear();
    mockWordFromHex.mockClear();
  });

  it('derives serial number from salt felts plus four zero felts', () => {
    const salt = { toHex: () => '0x' + '11'.repeat(32) } as unknown as Word;

    buildP2idTransactionRequest(
      '0x7bfb0f38b0fafa103f86a805594170',
      '0x8a65fc5a39e4cd106d648e3eb4ab5f',
      FAUCET_ID,
      10n,
      mockAccount,
      { salt },
    );

    expect(mockRandomWord).not.toHaveBeenCalled();
    expect(mockHashElements).toHaveBeenCalledTimes(1);

    const [feltArrayArg] = mockHashElements.mock.calls[0] as [{ values: unknown[] }];
    const values = feltArrayArg.values;

    expect(values).toHaveLength(8);
    expect(values.slice(0, 4)).toEqual(saltFelts);

    for (const felt of values.slice(4)) {
      expect((felt as { value: bigint }).value).toBe(0n);
    }
  });

  it('builds the asset from the matching vault entry via fromVaultKey', () => {
    const salt = { toHex: () => '0x' + '11'.repeat(32) } as unknown as Word;

    buildP2idTransactionRequest(
      '0x7bfb0f38b0fafa103f86a805594170',
      '0x8a65fc5a39e4cd106d648e3eb4ab5f',
      FAUCET_ID,
      10n,
      mockAccount,
      { salt },
    );

    expect(mockFromVaultKey).toHaveBeenCalledTimes(1);
    expect(mockFromVaultKey).toHaveBeenCalledWith({ kind: 'vault-key' }, 10n);
  });

  it('throws when the faucet has no entry in the account vault', () => {
    const salt = { toHex: () => '0x' + '11'.repeat(32) } as unknown as Word;
    const emptyVaultAccount = {
      vault: () => ({ fungibleAssets: () => [] }),
    } as unknown as Account;

    expect(() => buildP2idTransactionRequest(
      '0x7bfb0f38b0fafa103f86a805594170',
      '0x8a65fc5a39e4cd106d648e3eb4ab5f',
      FAUCET_ID,
      10n,
      emptyVaultAccount,
      { salt },
    )).toThrow('Asset not found in vault, cannot do P2ID transaction');
    expect(mockFromVaultKey).not.toHaveBeenCalled();
    expect(mockFungibleAssetConstructor).not.toHaveBeenCalled();
  });
});
