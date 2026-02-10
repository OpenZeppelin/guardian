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

  static validatePublicKeyHex(publicKeyHex: string): boolean {
    const clean = ensureHexPrefix(publicKeyHex).slice(2);
    const byteLength = clean.length / 2;
    return byteLength === 33 || byteLength === 65;
  }
}
