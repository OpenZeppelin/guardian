import { describe, it, expect, vi, beforeAll } from 'vitest';

// Mock Miden SDK before importing transaction module
vi.mock('@demox-labs/miden-sdk', () => ({
  AccountId: { fromHex: vi.fn() },
  AdviceMap: vi.fn(),
  Felt: vi.fn().mockImplementation((v) => ({ value: v })),
  FeltArray: vi.fn().mockImplementation((arr) => arr),
  Rpo256: { hashElements: vi.fn() },
  Signature: { deserialize: vi.fn() },
  TransactionRequestBuilder: vi.fn().mockImplementation(() => ({
    withCustomScript: vi.fn().mockReturnThis(),
    withScriptArg: vi.fn().mockReturnThis(),
    extendAdviceMap: vi.fn().mockReturnThis(),
    withAuthArg: vi.fn().mockReturnThis(),
    build: vi.fn().mockReturnValue({}),
  })),
  Word: {
    fromHex: vi.fn().mockImplementation((hex: string) => ({
      toHex: () => hex,
      toFelts: () => [1, 2, 3, 4],
    })),
  },
}));

// Now import the module under test - use dynamic import to ensure mocks are in place
let normalizeHexWord: typeof import('./utils/encoding.js').normalizeHexWord;
let signatureHexToBytes: typeof import('./utils/signature.js').signatureHexToBytes;
let hexToBytes: typeof import('./utils/encoding.js').hexToBytes;

beforeAll(async () => {
  const encoding = await import('./utils/encoding.js');
  const sig = await import('./utils/signature.js');
  normalizeHexWord = encoding.normalizeHexWord;
  hexToBytes = encoding.hexToBytes;
  signatureHexToBytes = sig.signatureHexToBytes;
});

describe('transaction utilities', () => {
  describe('normalizeHexWord', () => {
    it('should handle hex without 0x prefix', () => {
      const result = normalizeHexWord('abc123');
      expect(result).toBe('0x' + '0'.repeat(58) + 'abc123');
    });

    it('should handle hex with 0x prefix', () => {
      const result = normalizeHexWord('0xabc123');
      expect(result).toBe('0x' + '0'.repeat(58) + 'abc123');
    });

    it('should handle hex with 0X prefix', () => {
      const result = normalizeHexWord('0Xabc123');
      expect(result).toBe('0x' + '0'.repeat(58) + 'abc123');
    });

    it('should convert to lowercase', () => {
      const result = normalizeHexWord('0xABC123DEF');
      expect(result).toBe('0x' + '0'.repeat(55) + 'abc123def');
    });

    it('should pad to 64 characters', () => {
      const result = normalizeHexWord('1');
      expect(result).toBe('0x' + '0'.repeat(63) + '1');
      expect(result.length).toBe(66); // 0x + 64 chars
    });

    it('should preserve full 64-char hex', () => {
      const fullHex = 'a'.repeat(64);
      const result = normalizeHexWord(fullHex);
      expect(result).toBe('0x' + fullHex);
    });

    it('should preserve full 64-char hex with prefix', () => {
      const fullHex = 'b'.repeat(64);
      const result = normalizeHexWord('0x' + fullHex);
      expect(result).toBe('0x' + fullHex);
    });
  });

  describe('hexToBytes', () => {
    it('should convert hex string to Uint8Array', () => {
      const result = hexToBytes('deadbeef');
      expect(result).toEqual(new Uint8Array([0xde, 0xad, 0xbe, 0xef]));
    });

    it('should handle 0x prefix', () => {
      const result = hexToBytes('0xdeadbeef');
      expect(result).toEqual(new Uint8Array([0xde, 0xad, 0xbe, 0xef]));
    });

    it('should handle empty string', () => {
      const result = hexToBytes('');
      expect(result).toEqual(new Uint8Array([]));
    });

    it('should handle single byte', () => {
      const result = hexToBytes('ff');
      expect(result).toEqual(new Uint8Array([0xff]));
    });

    it('should handle zeros', () => {
      const result = hexToBytes('00000000');
      expect(result).toEqual(new Uint8Array([0, 0, 0, 0]));
    });

    it('should handle mixed case', () => {
      const result = hexToBytes('DeAdBeEf');
      expect(result).toEqual(new Uint8Array([0xde, 0xad, 0xbe, 0xef]));
    });
  });

  describe('signatureHexToBytes', () => {
    it('should prepend auth scheme byte (0 = RpoFalcon512)', () => {
      const result = signatureHexToBytes('deadbeef');
      expect(result[0]).toBe(0); // RpoFalcon512 scheme byte
      expect(result.slice(1)).toEqual(new Uint8Array([0xde, 0xad, 0xbe, 0xef]));
    });

    it('should prepend auth scheme byte (1 = ECDSA)', () => {
      const result = signatureHexToBytes('deadbeef', 'ecdsa');
      expect(result[0]).toBe(1); // ECDSA scheme byte
      expect(result.slice(1)).toEqual(new Uint8Array([0xde, 0xad, 0xbe, 0xef]));
    });

    it('should handle 0x prefix', () => {
      const result = signatureHexToBytes('0xaabbccdd');
      expect(result[0]).toBe(0);
      expect(result.slice(1)).toEqual(new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd]));
    });

    it('should return correct length', () => {
      const sigHex = 'a'.repeat(128); // 64 bytes
      const result = signatureHexToBytes(sigHex);
      expect(result.length).toBe(65); // 64 bytes + 1 prefix byte
    });

    it('should handle empty signature', () => {
      const result = signatureHexToBytes('');
      expect(result).toEqual(new Uint8Array([0])); // Just the prefix
    });
  });
});
