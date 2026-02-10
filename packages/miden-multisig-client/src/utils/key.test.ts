import { describe, it, expect, vi } from 'vitest';
import { PublicKeyFormat } from './key.js';

vi.mock('@miden-sdk/miden-sdk', () => ({
  Word: class {
    private u64s: BigUint64Array;
    constructor(u64s: BigUint64Array) {
      this.u64s = u64s;
    }
    toHex() {
      const h3 = this.u64s[3].toString(16).padStart(16, '0');
      const h2 = this.u64s[2].toString(16).padStart(16, '0');
      const h1 = this.u64s[1].toString(16).padStart(16, '0');
      const h0 = this.u64s[0].toString(16).padStart(16, '0');
      return '0x' + h3 + h2 + h1 + h0;
    }
  },
}));

vi.mock('./signature.js', () => ({
  tryComputeCommitmentHex: vi.fn((_hex: string, _scheme: string) => '0x' + 'cc'.repeat(32)),
}));

describe('PublicKeyFormat', () => {
  describe('parse', () => {
    it('should treat 32-byte input as Word commitment with falcon scheme', () => {
      const bytes = new Uint8Array(32);
      bytes[0] = 0x42;
      const result = PublicKeyFormat.parse(bytes);
      expect(result.scheme).toBe('falcon');
      expect(result.commitment).toBe(result.publicKeyHex);
      expect(result.publicKeyHex).toMatch(/^0x[a-f0-9]{64}$/);
    });

    it('should detect 0x00-prefixed key as falcon', () => {
      const bytes = new Uint8Array(200);
      bytes[0] = 0x00;
      bytes[1] = 0xab;
      const result = PublicKeyFormat.parse(bytes);
      expect(result.scheme).toBe('falcon');
      expect(result.commitment).toBe('0x' + 'cc'.repeat(32));
    });

    it('should detect 0x01-prefixed 34-byte key as ecdsa', () => {
      const bytes = new Uint8Array(34);
      bytes[0] = 0x01;
      bytes[1] = 0x02;
      const result = PublicKeyFormat.parse(bytes);
      expect(result.scheme).toBe('ecdsa');
      expect(result.commitment).toBe('0x' + 'cc'.repeat(32));
    });

    it('should detect 33-byte input as ecdsa', () => {
      const bytes = new Uint8Array(33);
      bytes[0] = 0x02;
      const result = PublicKeyFormat.parse(bytes);
      expect(result.scheme).toBe('ecdsa');
    });

    it('should detect 65-byte input as ecdsa', () => {
      const bytes = new Uint8Array(65);
      bytes[0] = 0x04;
      const result = PublicKeyFormat.parse(bytes);
      expect(result.scheme).toBe('ecdsa');
    });

    it('should default to falcon for unknown lengths', () => {
      const bytes = new Uint8Array(500);
      const result = PublicKeyFormat.parse(bytes);
      expect(result.scheme).toBe('falcon');
    });
  });

  describe('wordBytesToHex', () => {
    it('should convert 32 zero bytes to a valid hex string', () => {
      const bytes = new Uint8Array(32);
      const result = PublicKeyFormat.wordBytesToHex(bytes);
      expect(result).toMatch(/^0x[a-f0-9]{64}$/);
    });

    it('should produce consistent output for same input', () => {
      const bytes = new Uint8Array(32);
      bytes[0] = 0xff;
      const a = PublicKeyFormat.wordBytesToHex(bytes);
      const b = PublicKeyFormat.wordBytesToHex(bytes);
      expect(a).toBe(b);
    });
  });
});
