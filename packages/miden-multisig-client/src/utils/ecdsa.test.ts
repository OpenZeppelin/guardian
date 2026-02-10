import { describe, it, expect } from 'vitest';
import { EcdsaFormat } from './ecdsa.js';

describe('EcdsaFormat', () => {
  describe('normalizeRecoveryByte', () => {
    it('should normalize v=27 to recovery_id=0', () => {
      const rHex = 'aa'.repeat(32);
      const sHex = 'bb'.repeat(32);
      const sig = `0x${rHex}${sHex}1b`;
      const result = EcdsaFormat.normalizeRecoveryByte(sig);
      expect(result).toBe(`0x${rHex}${sHex}00`);
    });

    it('should normalize v=28 to recovery_id=1', () => {
      const rHex = 'aa'.repeat(32);
      const sHex = 'bb'.repeat(32);
      const sig = `0x${rHex}${sHex}1c`;
      const result = EcdsaFormat.normalizeRecoveryByte(sig);
      expect(result).toBe(`0x${rHex}${sHex}01`);
    });

    it('should pass through v=0 unchanged', () => {
      const rHex = 'aa'.repeat(32);
      const sHex = 'bb'.repeat(32);
      const sig = `0x${rHex}${sHex}00`;
      const result = EcdsaFormat.normalizeRecoveryByte(sig);
      expect(result).toBe(sig);
    });

    it('should pass through 64-byte signature unchanged', () => {
      const sig = `0x${'ab'.repeat(64)}`;
      const result = EcdsaFormat.normalizeRecoveryByte(sig);
      expect(result).toBe(sig);
    });

    it('should throw on invalid length', () => {
      expect(() => EcdsaFormat.normalizeRecoveryByte('0x' + 'ab'.repeat(10))).toThrow(
        'Invalid ECDSA signature length',
      );
    });

    it('should handle input without 0x prefix', () => {
      const rHex = 'aa'.repeat(32);
      const sHex = 'bb'.repeat(32);
      const sig = `${rHex}${sHex}1b`;
      const result = EcdsaFormat.normalizeRecoveryByte(sig);
      expect(result).toBe(`0x${rHex}${sHex}00`);
    });
  });

  describe('compressPublicKey', () => {
    it('should compress a 65-byte uncompressed key with even y', () => {
      const x = 'aa'.repeat(32);
      const yEven = 'bb'.repeat(31) + 'b0';
      const uncompressed = `0x04${x}${yEven}`;
      const result = EcdsaFormat.compressPublicKey(uncompressed);
      expect(result).toBe(`0x02${x}`);
    });

    it('should compress a 65-byte uncompressed key with odd y', () => {
      const x = 'aa'.repeat(32);
      const yOdd = 'bb'.repeat(31) + 'b1';
      const uncompressed = `0x04${x}${yOdd}`;
      const result = EcdsaFormat.compressPublicKey(uncompressed);
      expect(result).toBe(`0x03${x}`);
    });

    it('should pass through a 33-byte compressed key unchanged', () => {
      const compressed = `0x02${'aa'.repeat(32)}`;
      const result = EcdsaFormat.compressPublicKey(compressed);
      expect(result).toBe(compressed);
    });

    it('should throw on invalid length', () => {
      expect(() => EcdsaFormat.compressPublicKey('0x' + 'ab'.repeat(10))).toThrow(
        'Expected 65-byte uncompressed public key',
      );
    });

    it('should throw on 65 bytes without 04 prefix', () => {
      expect(() => EcdsaFormat.compressPublicKey('0x05' + 'ab'.repeat(64))).toThrow(
        'Expected 65-byte uncompressed public key',
      );
    });
  });

  describe('keccakDigestHex', () => {
    it('should return 0x-prefixed hex of keccak-256 hash', () => {
      const result = EcdsaFormat.keccakDigestHex(new Uint8Array(0));
      expect(result).toMatch(/^0x[a-f0-9]{64}$/);
    });

    it('should produce correct hash for empty input', () => {
      const result = EcdsaFormat.keccakDigestHex(new Uint8Array(0));
      expect(result).toBe('0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470');
    });

    it('should produce different hashes for different inputs', () => {
      const a = EcdsaFormat.keccakDigestHex(new Uint8Array([1]));
      const b = EcdsaFormat.keccakDigestHex(new Uint8Array([2]));
      expect(a).not.toBe(b);
    });
  });
});
