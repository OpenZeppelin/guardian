import { describe, it, expect } from 'vitest';
import { secp256k1 } from '@noble/curves/secp256k1';
import { keccak_256 } from '@noble/hashes/sha3.js';
import { EcdsaFormat } from './ecdsa.js';
import { bytesToHex } from './encoding.js';

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

  describe('isValidPublicKeyPoint', () => {
    // The secp256k1 generator point G, compressed — a known-valid point.
    const G = '0x0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798';

    it('accepts a valid compressed secp256k1 point', () => {
      expect(EcdsaFormat.isValidPublicKeyPoint(G)).toBe(true);
    });

    it('rejects a valid-length key whose x is off-curve', () => {
      expect(EcdsaFormat.isValidPublicKeyPoint('0x02' + 'ff'.repeat(32))).toBe(false);
    });

    it('rejects a valid-length key with a bad prefix byte', () => {
      expect(EcdsaFormat.isValidPublicKeyPoint('0xff' + 'ab'.repeat(32))).toBe(false);
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

  describe('recoverCompressedPublicKeyHex', () => {
    const privateKey = new Uint8Array(32).fill(0x11);
    const message = new Uint8Array([9, 8, 7, 6, 5]);

    function sign(priv: Uint8Array, msg: Uint8Array): Uint8Array {
      const sig = secp256k1.sign(keccak_256(msg), priv);
      const out = new Uint8Array(65);
      out.set(sig.toCompactRawBytes(), 0); // r || s
      out[64] = sig.recovery; // v
      return out;
    }

    it('recovers the compressed public key that produced the signature', () => {
      const expected = bytesToHex(secp256k1.getPublicKey(privateKey, true));
      const recovered = EcdsaFormat.recoverCompressedPublicKeyHex(message, sign(privateKey, message));
      expect(recovered).toBe(expected);
    });

    it('accepts an Ethereum-style recovery byte (27/28)', () => {
      const sig = sign(privateKey, message);
      sig[64] += 27; // 0/1 -> 27/28
      const expected = bytesToHex(secp256k1.getPublicKey(privateKey, true));
      expect(EcdsaFormat.recoverCompressedPublicKeyHex(message, sig)).toBe(expected);
    });

    it('throws when the signature is not 65 bytes', () => {
      expect(() => EcdsaFormat.recoverCompressedPublicKeyHex(message, new Uint8Array(64))).toThrow(
        /65-byte signature/,
      );
    });

    it('throws on an invalid recovery byte', () => {
      const sig = sign(privateKey, message);
      sig[64] = 5;
      expect(() => EcdsaFormat.recoverCompressedPublicKeyHex(message, sig)).toThrow(
        /recovery byte must be/,
      );
    });
  });
});
