import { describe, it, expect, vi, beforeEach } from 'vitest';
import { signatureHexToBytes, buildSignatureAdviceEntry, mergeSignatureAdviceMaps, toWord } from './signature.js';

// Mock the Miden SDK
vi.mock('@demox-labs/miden-sdk', () => ({
  AdviceMap: vi.fn().mockImplementation(() => ({
    insert: vi.fn(),
  })),
  Felt: vi.fn().mockImplementation((v: bigint) => ({ value: v })),
  FeltArray: vi.fn().mockImplementation((arr: any[]) => arr),
  Rpo256: {
    hashElements: vi.fn().mockReturnValue({
      toHex: () => '0x' + 'h'.repeat(64),
    }),
  },
  Signature: {
    deserialize: vi.fn().mockReturnValue({
      toPreparedSignature: vi.fn().mockReturnValue([1, 2, 3, 4]),
    }),
  },
  Word: {
    fromHex: vi.fn((hex: string) => ({
      toHex: () => hex,
      toFelts: () => [
        { value: 1n },
        { value: 2n },
        { value: 3n },
        { value: 4n },
      ],
    })),
  },
}));

describe('signature utilities', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  describe('signatureHexToBytes', () => {
    it('prepends auth scheme byte (0 = RpoFalcon512)', () => {
      const result = signatureHexToBytes('deadbeef');
      expect(result[0]).toBe(0); // RpoFalcon512 scheme byte
    });

    it('converts hex to bytes after prefix', () => {
      const result = signatureHexToBytes('deadbeef');
      expect(result.slice(1)).toEqual(new Uint8Array([0xde, 0xad, 0xbe, 0xef]));
    });

    it('handles 0x prefix', () => {
      const result = signatureHexToBytes('0xaabbccdd');
      expect(result[0]).toBe(0);
      expect(result.slice(1)).toEqual(new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd]));
    });

    it('returns correct length (input bytes + 1)', () => {
      const sigHex = 'a'.repeat(128); // 64 bytes
      const result = signatureHexToBytes(sigHex);
      expect(result.length).toBe(65); // 64 bytes + 1 prefix byte
    });

    it('handles empty signature', () => {
      const result = signatureHexToBytes('');
      expect(result).toEqual(new Uint8Array([0])); // Just the prefix
    });

    it('handles empty with 0x prefix', () => {
      const result = signatureHexToBytes('0x');
      expect(result).toEqual(new Uint8Array([0]));
    });
  });

  describe('buildSignatureAdviceEntry', () => {
    it('returns key and values', async () => {
      const { Word, Signature } = await import('@demox-labs/miden-sdk');
      const pubkeyCommitment = Word.fromHex('0x' + 'a'.repeat(64));
      const message = Word.fromHex('0x' + 'b'.repeat(64));
      const signature = Signature.deserialize(new Uint8Array([0, 1, 2, 3]));

      const { key, values } = buildSignatureAdviceEntry(pubkeyCommitment, message, signature);

      expect(key).toBeDefined();
      expect(values).toBeDefined();
    });

    it('hashes pubkey commitment and message to produce key', async () => {
      const { Word, Signature, Rpo256 } = await import('@demox-labs/miden-sdk');
      const pubkeyCommitment = Word.fromHex('0x' + 'a'.repeat(64));
      const message = Word.fromHex('0x' + 'b'.repeat(64));
      const signature = Signature.deserialize(new Uint8Array([0, 1, 2, 3]));

      buildSignatureAdviceEntry(pubkeyCommitment, message, signature);

      expect(Rpo256.hashElements).toHaveBeenCalled();
    });

    it('uses toPreparedSignature to get values', async () => {
      const { Word, Signature } = await import('@demox-labs/miden-sdk');
      const pubkeyCommitment = Word.fromHex('0x' + 'a'.repeat(64));
      const message = Word.fromHex('0x' + 'b'.repeat(64));
      const signature = Signature.deserialize(new Uint8Array([0, 1, 2, 3]));

      const { values } = buildSignatureAdviceEntry(pubkeyCommitment, message, signature);

      expect(signature.toPreparedSignature).toHaveBeenCalledWith(message);
      expect(values).toEqual([1, 2, 3, 4]);
    });
  });

  describe('mergeSignatureAdviceMaps', () => {
    it('inserts all entries into advice map', async () => {
      const { AdviceMap, Word, Signature } = await import('@demox-labs/miden-sdk');
      const advice = new AdviceMap();

      const entries = [
        {
          key: Word.fromHex('0x' + 'a'.repeat(64)),
          values: [{ value: 1n }, { value: 2n }],
        },
        {
          key: Word.fromHex('0x' + 'b'.repeat(64)),
          values: [{ value: 3n }, { value: 4n }],
        },
      ];

      const result = mergeSignatureAdviceMaps(advice, entries as any);

      expect(advice.insert).toHaveBeenCalledTimes(2);
      expect(result).toBe(advice);
    });

    it('returns the same advice map', async () => {
      const { AdviceMap } = await import('@demox-labs/miden-sdk');
      const advice = new AdviceMap();

      const result = mergeSignatureAdviceMaps(advice, []);

      expect(result).toBe(advice);
    });

    it('handles empty entries array', async () => {
      const { AdviceMap } = await import('@demox-labs/miden-sdk');
      const advice = new AdviceMap();

      mergeSignatureAdviceMaps(advice, []);

      expect(advice.insert).not.toHaveBeenCalled();
    });
  });

  describe('toWord', () => {
    it('creates Word from hex string', async () => {
      const { Word } = await import('@demox-labs/miden-sdk');

      const word = toWord('abc123');

      expect(Word.fromHex).toHaveBeenCalled();
      expect(word).toBeDefined();
    });

    it('normalizes hex before creating Word', async () => {
      const { Word } = await import('@demox-labs/miden-sdk');

      toWord('abc');

      // Should be called with normalized hex (0x + padded to 64 chars)
      expect(Word.fromHex).toHaveBeenCalledWith('0x' + '0'.repeat(61) + 'abc');
    });

    it('handles 0x prefix', async () => {
      const { Word } = await import('@demox-labs/miden-sdk');

      toWord('0xdef');

      expect(Word.fromHex).toHaveBeenCalledWith('0x' + '0'.repeat(61) + 'def');
    });

    it('handles full 64-char hex', async () => {
      const { Word } = await import('@demox-labs/miden-sdk');
      const fullHex = 'f'.repeat(64);

      toWord(fullHex);

      expect(Word.fromHex).toHaveBeenCalledWith('0x' + fullHex);
    });
  });
});
