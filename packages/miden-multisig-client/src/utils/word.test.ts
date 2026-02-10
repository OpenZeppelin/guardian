import { describe, it, expect, vi } from 'vitest';
import { wordElementToBigInt, wordToHex, wordToBytes } from './word.js';
import { Word } from '@miden-sdk/miden-sdk';

// Mock the Miden SDK Word class
vi.mock('@miden-sdk/miden-sdk', () => ({
  Word: {
    fromHex: (hex: string): { toU64s: () => BigUint64Array; toHex: () => string } => {
      // Parse hex string (remove 0x prefix if present)
      const cleanHex = hex.startsWith('0x') ? hex.slice(2) : hex;
      if (cleanHex.length !== 64) {
        throw new Error('Hex string must be 64 characters');
      }

      // Parse as big-endian: [e3_bytes][e2_bytes][e1_bytes][e0_bytes]
      const e3 = BigInt('0x' + cleanHex.slice(0, 16));
      const e2 = BigInt('0x' + cleanHex.slice(16, 32));
      const e1 = BigInt('0x' + cleanHex.slice(32, 48));
      const e0 = BigInt('0x' + cleanHex.slice(48, 64));

      const elements = new BigUint64Array([e0, e1, e2, e3]);

      return {
        toU64s: () => elements,
        toHex: () => {
          const h3 = elements[3].toString(16).padStart(16, '0');
          const h2 = elements[2].toString(16).padStart(16, '0');
          const h1 = elements[1].toString(16).padStart(16, '0');
          const h0 = elements[0].toString(16).padStart(16, '0');
          return '0x' + h3 + h2 + h1 + h0;
        },
      };
    },
  },
}));

describe('word utilities', () => {
  describe('wordElementToBigInt', () => {
    it('should return element at valid index', () => {
      const word = Word.fromHex(
        '0x016ab79593165e5b849776919e0c0298fb9dac880d593d93edd7134bdcdb4b6f'
      );
      expect(wordElementToBigInt(word as Word, 0)).toBe(BigInt('0xedd7134bdcdb4b6f'));
      expect(wordElementToBigInt(word as Word, 3)).toBe(BigInt('0x016ab79593165e5b'));
    });

    it('should return 0n for invalid index', () => {
      const word = Word.fromHex(
        '0x016ab79593165e5b849776919e0c0298fb9dac880d593d93edd7134bdcdb4b6f'
      );
      expect(wordElementToBigInt(word as Word, -1)).toBe(0n);
      expect(wordElementToBigInt(word as Word, 4)).toBe(0n);
    });
  });

  describe('wordToHex', () => {
    it('should convert word back to hex string', () => {
      const originalHex = '0xd6c130dba13c67ac4733915f24bea9d19f517f51a65c74ded7bcd27e066b400e';
      const word = Word.fromHex(originalHex);
      const hex = wordToHex(word as Word);
      expect(hex).toBe(originalHex);
    });
  });

  describe('wordToBytes', () => {
    it('should convert a word with known felt values to LE bytes', () => {
      const word = {
        toFelts: () => [
          { asInt: () => 1n },
          { asInt: () => 2n },
          { asInt: () => 3n },
          { asInt: () => 4n },
        ],
      };
      const bytes = wordToBytes(word);
      expect(bytes.length).toBe(32);
      expect(bytes[0]).toBe(1);
      expect(bytes[1]).toBe(0);
      expect(bytes[8]).toBe(2);
      expect(bytes[16]).toBe(3);
      expect(bytes[24]).toBe(4);
    });

    it('should encode felts in little-endian order', () => {
      const word = {
        toFelts: () => [
          { asInt: () => 0x0102030405060708n },
          { asInt: () => 0n },
          { asInt: () => 0n },
          { asInt: () => 0n },
        ],
      };
      const bytes = wordToBytes(word);
      expect(bytes[0]).toBe(0x08);
      expect(bytes[1]).toBe(0x07);
      expect(bytes[2]).toBe(0x06);
      expect(bytes[3]).toBe(0x05);
      expect(bytes[4]).toBe(0x04);
      expect(bytes[5]).toBe(0x03);
      expect(bytes[6]).toBe(0x02);
      expect(bytes[7]).toBe(0x01);
    });

    it('should handle zero felts', () => {
      const word = {
        toFelts: () => [
          { asInt: () => 0n },
          { asInt: () => 0n },
          { asInt: () => 0n },
          { asInt: () => 0n },
        ],
      };
      const bytes = wordToBytes(word);
      expect(bytes.length).toBe(32);
      expect(bytes.every((b) => b === 0)).toBe(true);
    });
  });
});
