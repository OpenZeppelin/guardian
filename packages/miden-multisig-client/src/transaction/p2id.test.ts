import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { Word } from '@miden-sdk/miden-sdk';

const {
  mockFungibleAssetConstructor,
  mockHashElements,
  mockNormalizeHexWord,
  mockRandomWord,
  mockWordFromHex,
  noteMetadataCalls,
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
    noteMetadataCalls: [] as unknown[][],
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
      sender: unknown,
      noteType: unknown,
      noteTag: unknown,
    ) {
      noteMetadataCalls.push([sender, noteType, noteTag]);
    }
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
      Private: 0,
      Public: 1,
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

import { buildP2idTransactionRequest, parseP2idNoteType, p2idNoteTypeToMetadata } from './p2id.js';
import { NoteType } from '@miden-sdk/miden-sdk';

const FAUCET_ID = '0x7bfb0f38b0fafa103f86a805594171';

describe('buildP2idTransactionRequest', () => {
  beforeEach(() => {
    mockFungibleAssetConstructor.mockClear();
    mockHashElements.mockClear();
    mockNormalizeHexWord.mockClear();
    mockRandomWord.mockClear();
    mockWordFromHex.mockClear();
    noteMetadataCalls.length = 0;
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
