import { describe, it, expect, vi } from 'vitest';
import { wordElementToBigInt, wordToHex } from './word.js';
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
});
