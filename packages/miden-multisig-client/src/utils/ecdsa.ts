import { keccak_256 } from '@noble/hashes/sha3.js';
import { ensureHexPrefix } from './encoding.js';

const ECDSA_SIGNATURE_BYTE_LENGTH = 64;
const ECDSA_SIGNATURE_WITH_RECOVERY_BYTE_LENGTH = 65;

export class EcdsaFormat {
  static normalizeSignatureHex(signatureHex: string): string {
    const clean = ensureHexPrefix(signatureHex).slice(2);
    const byteLength = clean.length / 2;

    if (byteLength === ECDSA_SIGNATURE_WITH_RECOVERY_BYTE_LENGTH) {
      return `0x${clean.slice(0, ECDSA_SIGNATURE_BYTE_LENGTH * 2)}`;
    }

    if (byteLength === ECDSA_SIGNATURE_BYTE_LENGTH) {
      return `0x${clean}`;
    }

    throw new Error(
      `Invalid ECDSA signature length: expected ${ECDSA_SIGNATURE_BYTE_LENGTH} or ${ECDSA_SIGNATURE_WITH_RECOVERY_BYTE_LENGTH} bytes, got ${byteLength}`,
    );
  }

  static normalizeRecoveryByte(signatureHex: string): string {
    const clean = ensureHexPrefix(signatureHex).slice(2);
    const byteLength = clean.length / 2;

    if (byteLength === ECDSA_SIGNATURE_WITH_RECOVERY_BYTE_LENGTH) {
      const vByte = parseInt(clean.slice(-2), 16);
      if (vByte === 27 || vByte === 28) {
        const normalized = clean.slice(0, -2) + (vByte - 27).toString(16).padStart(2, '0');
        return `0x${normalized}`;
      }
      return `0x${clean}`;
    }

    if (byteLength === ECDSA_SIGNATURE_BYTE_LENGTH) {
      return `0x${clean}`;
    }

    throw new Error(
      `Invalid ECDSA signature length: expected ${ECDSA_SIGNATURE_BYTE_LENGTH} or ${ECDSA_SIGNATURE_WITH_RECOVERY_BYTE_LENGTH} bytes, got ${byteLength}`,
    );
  }

  static validatePublicKeyHex(publicKeyHex: string): boolean {
    const clean = ensureHexPrefix(publicKeyHex).slice(2);
    const byteLength = clean.length / 2;
    return byteLength === 33 || byteLength === 65;
  }

  static compressPublicKey(uncompressedHex: string): string {
    const clean = ensureHexPrefix(uncompressedHex).slice(2);
    const byteLength = clean.length / 2;

    if (byteLength === 33) return `0x${clean}`;

    if (byteLength !== 65 || !clean.startsWith('04')) {
      throw new Error(`Expected 65-byte uncompressed public key, got ${byteLength} bytes`);
    }

    const x = clean.slice(2, 66);
    const yLastNibble = parseInt(clean.slice(-1), 16);
    const tag = yLastNibble % 2 === 0 ? '02' : '03';
    return `0x${tag}${x}`;
  }

  static keccakDigestHex(data: Uint8Array): string {
    const hash = keccak_256(data);
    let hex = '0x';
    for (let i = 0; i < hash.length; i++) {
      hex += hash[i].toString(16).padStart(2, '0');
    }
    return hex;
  }
}
