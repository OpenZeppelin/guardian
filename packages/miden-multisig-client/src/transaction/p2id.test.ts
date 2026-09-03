import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { Word } from '@miden-sdk/miden-sdk';

const {
  mockFungibleAssetConstructor,
  mockWithAuthArg,
  mockExtendAdviceMap,
  mockHashElements,
  mockNormalizeHexWord,
  mockRandomWord,
  mockWordFromHex,
  noteMetadataCalls,
  noteRecipientCalls,
  noteStorageCalls,
  saltFelts,
} = vi.hoisted(() => {
  const saltFelts = [
    { id: 'felt-0' },
    { id: 'felt-1' },
    { id: 'felt-2' },
    { id: 'felt-3' },
  ];

  return {
    mockFungibleAssetConstructor: vi.fn(),
    mockWithAuthArg: vi.fn(),
    mockExtendAdviceMap: vi.fn(),
    noteMetadataCalls: [] as unknown[][],
    noteRecipientCalls: [] as unknown[][],
    noteStorageCalls: [] as unknown[][],
    mockHashElements: vi.fn().mockReturnValue({
      toString: () => 'serial',
      // Also stands in for the fee-auth commitment, which must be distinguishable
      // from the bare salt the default path passes.
      toHex: () => '0xcommitment',
      toFelts: () => [{ id: 'h-0' }, { id: 'h-1' }, { id: 'h-2' }, { id: 'h-3' }],
    }),
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

  class AdviceMap {
    readonly entries: Array<[unknown, unknown]> = [];

    insert(key: unknown, value: unknown): void {
      this.entries.push([key, value]);
    }
  }

  class NoteStorage {
    constructor(inputs: FeltArray) {
      noteStorageCalls.push([inputs]);
    }
  }

  class NoteMetadata {
    constructor(
      sender: unknown,
      noteType: unknown,
      noteTag: unknown,
    ) {
      noteMetadataCalls.push([sender, noteType, noteTag]);
    }
  }

  class NoteRecipient {
    constructor(
      serialNum: unknown,
      noteScript: unknown,
      noteInputs: unknown,
    ) {
      noteRecipientCalls.push([serialNum, noteScript, noteInputs]);
    }
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
  }

  class NoteArray {
    constructor(_notes: unknown[]) {}
  }

  class TransactionRequestBuilder {
    withOwnOutputNotes(_notes: unknown): this {
      return this;
    }

    withAuthArg(authArg: unknown): this {
      mockWithAuthArg(authArg);
      return this;
    }

    extendAdviceMap(adviceMap: unknown): this {
      mockExtendAdviceMap(adviceMap);
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
      p2ide: vi.fn(() => ({ kind: 'p2ide-script' })),
    },
    NoteTag: {
      withAccountTarget: vi.fn(() => ({ kind: 'tag' })),
    },
    NoteType: {
      Private: 0,
      Public: 1,
    },
    OutputNote: {
      full: vi.fn((note: unknown) => ({ note })),
    },
    Poseidon2: {
      hashElements: mockHashElements,
    },
    AdviceMap,
    TransactionRequestBuilder,
    Word: {
      fromHex: mockWordFromHex,
      newFromFelts: vi.fn(() => ({
        toHex: () => '0xconversioninfo',
        toFelts: () => [{ id: 'conv-0' }, { id: 'conv-1' }, { id: 'conv-2' }, { id: 'conv-3' }],
      })),
    },
  };
});

vi.mock('../utils/encoding.js', () => ({
  normalizeHexWord: mockNormalizeHexWord,
}));

vi.mock('../utils/random.js', () => ({
  randomWord: mockRandomWord,
}));

import { buildP2idTransactionRequest, parseP2idNoteType, p2idNoteTypeToMetadata } from './p2id.js';
import { NoteType } from '@miden-sdk/miden-sdk';

const FAUCET_ID = '0x7bfb0f38b0fafa103f86a805594171';

describe('buildP2idTransactionRequest', () => {
  beforeEach(() => {
    mockFungibleAssetConstructor.mockClear();
    mockWithAuthArg.mockClear();
    mockExtendAdviceMap.mockClear();
    mockHashElements.mockClear();
    mockNormalizeHexWord.mockClear();
    mockRandomWord.mockClear();
    mockWordFromHex.mockClear();
    noteMetadataCalls.length = 0;
    noteRecipientCalls.length = 0;
    noteStorageCalls.length = 0;
  });

  it('passes the bare salt as the auth arg and adds no fee advice by default', () => {
    const salt = { toHex: () => '0x' + '11'.repeat(32) } as unknown as Word;

    buildP2idTransactionRequest(
      '0x7bfb0f38b0fafa103f86a805594170',
      '0x8a65fc5a39e4cd106d648e3eb4ab5f',
      FAUCET_ID,
      10n,
      { salt },
    );

    expect(mockWithAuthArg).toHaveBeenCalledTimes(1);
    const [authArg] = mockWithAuthArg.mock.calls[0] as [{ toHex: () => string }];
    expect(authArg.toHex()).toBe(salt.toHex());
    expect(mockExtendAdviceMap).not.toHaveBeenCalled();
  });

  it('commits to conversion info and adds its preimage when given a fee faucet', () => {
    // The only user-facing effect of SignatureOptions.feeFaucetId. Dropping the
    // advice entry while keeping the commitment would abort on-chain with
    // ERR_FEE_CONVERSION_INFO_MISSING rather than fail here.
    const salt = { toHex: () => '0x' + '11'.repeat(32) } as unknown as Word;

    buildP2idTransactionRequest(
      '0x7bfb0f38b0fafa103f86a805594170',
      '0x8a65fc5a39e4cd106d648e3eb4ab5f',
      FAUCET_ID,
      10n,
      { salt, feeFaucetId: FAUCET_ID },
    );

    expect(mockWithAuthArg).toHaveBeenCalledTimes(1);
    const [authArg] = mockWithAuthArg.mock.calls[0] as [{ toHex: () => string }];
    expect(authArg.toHex()).toBe('0xcommitment');

    // The note must still come from the salt. The commitment goes to the auth
    // arg only; routing it into the note builder as well would change the output
    // note — see tests/p2id-fee-note-binding.test.ts for what that costs.
    expect(mockWordFromHex).toHaveBeenCalledWith(salt.toHex());
    expect(mockWordFromHex).not.toHaveBeenCalledWith('0xcommitment');

    expect(mockExtendAdviceMap).toHaveBeenCalledTimes(1);
    const [adviceMap] = mockExtendAdviceMap.mock.calls[0] as [
      { entries: Array<[{ toHex: () => string }, { values: Array<{ id: string }> }]> },
    ];
    expect(adviceMap.entries).toHaveLength(1);
    const [key, preimage] = adviceMap.entries[0];
    expect(key.toHex()).toBe('0xcommitment');
    expect(preimage.values.map((felt) => felt.id)).toEqual([
      'felt-0',
      'felt-1',
      'felt-2',
      'felt-3',
      'conv-0',
      'conv-1',
      'conv-2',
      'conv-3',
    ]);
  });

  it('derives serial number from salt felts plus four zero felts', () => {
    const salt = { toHex: () => '0x' + '11'.repeat(32) } as unknown as Word;

    buildP2idTransactionRequest(
      '0x7bfb0f38b0fafa103f86a805594170',
      '0x8a65fc5a39e4cd106d648e3eb4ab5f',
      FAUCET_ID,
      10n,
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

  it('creates a public note by default (issue #322)', () => {
    buildP2idTransactionRequest(
      '0x7bfb0f38b0fafa103f86a805594170',
      '0x8a65fc5a39e4cd106d648e3eb4ab5f',
      FAUCET_ID,
      10n,
    );

    expect(noteMetadataCalls).toHaveLength(1);
    expect(noteMetadataCalls[0][1]).toBe(NoteType.Public);
  });

  it('builds the asset from the faucet id, whose callback flag it carries since Miden 0.16', () => {
    buildP2idTransactionRequest(
      '0x7bfb0f38b0fafa103f86a805594170',
      '0x8a65fc5a39e4cd106d648e3eb4ab5f',
      FAUCET_ID,
      10n,
    );

    expect(mockFungibleAssetConstructor).toHaveBeenCalledTimes(1);
    const [faucet, amount] = mockFungibleAssetConstructor.mock.calls[0] as [{ hex: string }, bigint];
    expect(faucet.hex).toBe(FAUCET_ID);
    expect(amount).toBe(10n);
  });

  it('threads the requested noteType into the note metadata (issue #322)', () => {
    buildP2idTransactionRequest(
      '0x7bfb0f38b0fafa103f86a805594170',
      '0x8a65fc5a39e4cd106d648e3eb4ab5f',
      '0x7bfb0f38b0fafa103f86a805594171',
      10n,
      { noteType: NoteType.Private },
    );

    expect(noteMetadataCalls).toHaveLength(1);
    expect(noteMetadataCalls[0][1]).toBe(NoteType.Private);
  });

  it('builds a plain P2ID note without heights: p2id script, 2 storage items', () => {
    buildP2idTransactionRequest(
      '0x7bfb0f38b0fafa103f86a805594170',
      '0x8a65fc5a39e4cd106d648e3eb4ab5f',
      FAUCET_ID,
      10n,
    );

    expect(noteRecipientCalls).toHaveLength(1);
    expect(noteRecipientCalls[0][1]).toEqual({ kind: 'p2id-script' });
    const [storageInputs] = noteStorageCalls[0] as [{ values: unknown[] }];
    expect(storageInputs.values).toEqual([2, 1]);
  });

  it('builds a P2IDE note when reclaimHeight is set (issue #366)', () => {
    buildP2idTransactionRequest(
      '0x7bfb0f38b0fafa103f86a805594170',
      '0x8a65fc5a39e4cd106d648e3eb4ab5f',
      FAUCET_ID,
      10n,
      { reclaimHeight: 12345 },
    );

    expect(noteRecipientCalls).toHaveLength(1);
    expect(noteRecipientCalls[0][1]).toEqual({ kind: 'p2ide-script' });

    // P2IDE storage layout: [suffix, prefix, reclaim, timelock], 0 = unset.
    const [storageInputs] = noteStorageCalls[0] as [{ values: unknown[] }];
    expect(storageInputs.values).toHaveLength(4);
    expect(storageInputs.values.slice(0, 2)).toEqual([2, 1]);
    expect((storageInputs.values[2] as { value: bigint }).value).toBe(12345n);
    expect((storageInputs.values[3] as { value: bigint }).value).toBe(0n);
  });

  it('builds a P2IDE note when only timelockHeight is set (issue #366)', () => {
    buildP2idTransactionRequest(
      '0x7bfb0f38b0fafa103f86a805594170',
      '0x8a65fc5a39e4cd106d648e3eb4ab5f',
      FAUCET_ID,
      10n,
      { timelockHeight: 777 },
    );

    expect(noteRecipientCalls[0][1]).toEqual({ kind: 'p2ide-script' });
    const [storageInputs] = noteStorageCalls[0] as [{ values: unknown[] }];
    expect((storageInputs.values[2] as { value: bigint }).value).toBe(0n);
    expect((storageInputs.values[3] as { value: bigint }).value).toBe(777n);
  });

  it('carries both heights into the P2IDE storage (issue #366)', () => {
    buildP2idTransactionRequest(
      '0x7bfb0f38b0fafa103f86a805594170',
      '0x8a65fc5a39e4cd106d648e3eb4ab5f',
      FAUCET_ID,
      10n,
      { reclaimHeight: 500, timelockHeight: 400 },
    );

    const [storageInputs] = noteStorageCalls[0] as [{ values: unknown[] }];
    expect((storageInputs.values[2] as { value: bigint }).value).toBe(500n);
    expect((storageInputs.values[3] as { value: bigint }).value).toBe(400n);
  });

  it.each([
    ['zero', 0],
    ['negative', -5],
    ['fractional', 1.5],
    ['above u32::MAX', 0x1_0000_0000],
  ])('rejects a %s height instead of building a divergent note (issue #366)', (_label, height) => {
    expect(() =>
      buildP2idTransactionRequest(
        '0x7bfb0f38b0fafa103f86a805594170',
        '0x8a65fc5a39e4cd106d648e3eb4ab5f',
        FAUCET_ID,
        10n,
          { reclaimHeight: height },
      ),
    ).toThrow(/unsupported reclaimHeight/);
  });
});

describe('parseP2idNoteType', () => {
  it('maps absent to Public (pre-#322 proposals)', () => {
    expect(parseP2idNoteType(undefined)).toBe(NoteType.Public);
  });

  it('maps wire values to note types', () => {
    expect(parseP2idNoteType('public')).toBe(NoteType.Public);
    expect(parseP2idNoteType('private')).toBe(NoteType.Private);
  });

  it('rejects unknown values instead of silently rebuilding a public note', () => {
    expect(() => parseP2idNoteType('encrypted')).toThrow(/unsupported metadata.noteType/);
  });
});

describe('p2idNoteTypeToMetadata', () => {
  it('omits the default so public payloads keep the legacy wire shape', () => {
    expect(p2idNoteTypeToMetadata(undefined)).toBeUndefined();
    expect(p2idNoteTypeToMetadata(NoteType.Public)).toBeUndefined();
  });

  it('serializes private', () => {
    expect(p2idNoteTypeToMetadata(NoteType.Private)).toBe('private');
  });
});
