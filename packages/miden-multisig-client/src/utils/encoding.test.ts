import { describe, it, expect } from 'vitest';
import {
  ensureHexPrefix,
  normalizeHexWord,
  bytesToHex,
  hexToBytes,
  uint8ArrayToBase64,
  base64ToUint8Array,
} from './encoding.js';

describe('encoding utilities', () => {
  describe('ensureHexPrefix', () => {
    it('adds 0x prefix when missing', () => {
      expect(ensureHexPrefix('abc123')).toBe('0xabc123');
    });

    it('preserves existing 0x prefix', () => {
      expect(ensureHexPrefix('0xabc123')).toBe('0xabc123');
    });

    it('preserves existing 0X prefix', () => {
      expect(ensureHexPrefix('0Xabc123')).toBe('0Xabc123');
    });

    it('handles empty string', () => {
      expect(ensureHexPrefix('')).toBe('0x');
    });

    it('handles single character', () => {
      expect(ensureHexPrefix('a')).toBe('0xa');
    });
  });

  describe('normalizeHexWord', () => {
    it('pads short hex to 64 characters', () => {
      const result = normalizeHexWord('abc123');
      expect(result).toBe('0x' + '0'.repeat(58) + 'abc123');
      expect(result.length).toBe(66); // 0x + 64 chars
    });

    it('preserves full 64-char hex', () => {
      const fullHex = 'a'.repeat(64);
      expect(normalizeHexWord(fullHex)).toBe('0x' + fullHex);
    });

    it('handles 0x prefix', () => {
      expect(normalizeHexWord('0xabc')).toBe('0x' + '0'.repeat(61) + 'abc');
    });

    it('handles 0X prefix', () => {
      expect(normalizeHexWord('0Xabc')).toBe('0x' + '0'.repeat(61) + 'abc');
    });

    it('converts to lowercase', () => {
      expect(normalizeHexWord('ABC')).toBe('0x' + '0'.repeat(61) + 'abc');
    });

    it('handles empty string', () => {
      expect(normalizeHexWord('')).toBe('0x' + '0'.repeat(64));
    });

    it('handles single digit', () => {
      expect(normalizeHexWord('1')).toBe('0x' + '0'.repeat(63) + '1');
    });

    it('handles full 64-char hex with prefix', () => {
      const fullHex = 'b'.repeat(64);
      expect(normalizeHexWord('0x' + fullHex)).toBe('0x' + fullHex);
    });
  });

  describe('bytesToHex', () => {
    it('converts Uint8Array to hex string', () => {
      const bytes = new Uint8Array([0xde, 0xad, 0xbe, 0xef]);
      expect(bytesToHex(bytes)).toBe('0xdeadbeef');
    });

    it('handles empty array', () => {
      expect(bytesToHex(new Uint8Array([]))).toBe('0x');
    });

    it('handles single byte', () => {
      expect(bytesToHex(new Uint8Array([0xff]))).toBe('0xff');
    });

    it('pads single digit bytes with zero', () => {
      const bytes = new Uint8Array([0x0a, 0x0b, 0x0c]);
      expect(bytesToHex(bytes)).toBe('0x0a0b0c');
    });

    it('handles zeros', () => {
      const bytes = new Uint8Array([0x00, 0x00, 0x00]);
      expect(bytesToHex(bytes)).toBe('0x000000');
    });

    it('handles max byte value', () => {
      const bytes = new Uint8Array([0xff, 0xff]);
      expect(bytesToHex(bytes)).toBe('0xffff');
    });

    it('handles long arrays', () => {
      const bytes = new Uint8Array(32).fill(0xaa);
      const result = bytesToHex(bytes);
      expect(result).toBe('0x' + 'aa'.repeat(32));
    });
  });

  describe('hexToBytes', () => {
    it('converts hex string to Uint8Array', () => {
      expect(hexToBytes('deadbeef')).toEqual(new Uint8Array([0xde, 0xad, 0xbe, 0xef]));
    });

    it('handles 0x prefix', () => {
      expect(hexToBytes('0xdeadbeef')).toEqual(new Uint8Array([0xde, 0xad, 0xbe, 0xef]));
    });

    it('handles 0X prefix', () => {
      expect(hexToBytes('0Xdeadbeef')).toEqual(new Uint8Array([0xde, 0xad, 0xbe, 0xef]));
    });

    it('handles empty string', () => {
      expect(hexToBytes('')).toEqual(new Uint8Array([]));
    });

    it('handles empty with prefix', () => {
      expect(hexToBytes('0x')).toEqual(new Uint8Array([]));
    });

    it('handles single byte', () => {
      expect(hexToBytes('ff')).toEqual(new Uint8Array([0xff]));
    });

    it('handles zeros', () => {
      expect(hexToBytes('00000000')).toEqual(new Uint8Array([0, 0, 0, 0]));
    });

    it('handles mixed case', () => {
      expect(hexToBytes('DeAdBeEf')).toEqual(new Uint8Array([0xde, 0xad, 0xbe, 0xef]));
    });

    it('handles long hex strings', () => {
      const hex = 'aa'.repeat(32);
      const result = hexToBytes(hex);
      expect(result).toEqual(new Uint8Array(32).fill(0xaa));
    });
  });

  describe('roundtrip bytesToHex/hexToBytes', () => {
    it('survives roundtrip bytes -> hex -> bytes', () => {
      const original = new Uint8Array([0xde, 0xad, 0xbe, 0xef, 0x01, 0x23, 0x45, 0x67]);
      const hex = bytesToHex(original);
      const roundtripped = hexToBytes(hex);
      expect(roundtripped).toEqual(original);
    });

    it('survives roundtrip hex -> bytes -> hex', () => {
      const original = '0xdeadbeef01234567';
      const bytes = hexToBytes(original);
      const roundtripped = bytesToHex(bytes);
      expect(roundtripped).toBe(original);
    });
  });

  describe('uint8ArrayToBase64', () => {
    it('converts Uint8Array to base64', () => {
      const bytes = new Uint8Array([72, 101, 108, 108, 111]); // "Hello"
      expect(uint8ArrayToBase64(bytes)).toBe('SGVsbG8=');
    });

    it('handles empty array', () => {
      expect(uint8ArrayToBase64(new Uint8Array([]))).toBe('');
    });

    it('handles binary data', () => {
      const bytes = new Uint8Array([0xff, 0x00, 0xaa, 0x55]);
      const base64 = uint8ArrayToBase64(bytes);
      expect(base64).toBeTruthy();
      // Verify by round-tripping
      expect(base64ToUint8Array(base64)).toEqual(bytes);
    });

    it('handles single byte', () => {
      const bytes = new Uint8Array([65]); // 'A'
      expect(uint8ArrayToBase64(bytes)).toBe('QQ==');
    });

    it('handles bytes requiring padding', () => {
      const bytes = new Uint8Array([1]);
      const base64 = uint8ArrayToBase64(bytes);
      expect(base64).toBe('AQ==');
    });
  });

  describe('base64ToUint8Array', () => {
    it('converts base64 to Uint8Array', () => {
      const base64 = 'SGVsbG8='; // "Hello"
      expect(base64ToUint8Array(base64)).toEqual(new Uint8Array([72, 101, 108, 108, 111]));
    });

    it('handles empty string', () => {
      expect(base64ToUint8Array('')).toEqual(new Uint8Array([]));
    });

    it('handles binary data', () => {
      const base64 = '/wCqVQ=='; // [0xff, 0x00, 0xaa, 0x55]
      expect(base64ToUint8Array(base64)).toEqual(new Uint8Array([0xff, 0x00, 0xaa, 0x55]));
    });

    it('handles base64 without padding', () => {
      const base64 = 'YWJj'; // "abc" - no padding needed
      expect(base64ToUint8Array(base64)).toEqual(new Uint8Array([97, 98, 99]));
    });
  });

  describe('roundtrip uint8ArrayToBase64/base64ToUint8Array', () => {
    it('survives roundtrip bytes -> base64 -> bytes', () => {
      const original = new Uint8Array([0, 1, 2, 3, 255, 254, 253, 252]);
      const base64 = uint8ArrayToBase64(original);
      const roundtripped = base64ToUint8Array(base64);
      expect(roundtripped).toEqual(original);
    });

    it('survives roundtrip for large arrays', () => {
      const original = new Uint8Array(1000);
      for (let i = 0; i < 1000; i++) {
        original[i] = i % 256;
      }
      const base64 = uint8ArrayToBase64(original);
      const roundtripped = base64ToUint8Array(base64);
      expect(roundtripped).toEqual(original);
    });
  });
});
