import { secp256k1 } from '@noble/curves/secp256k1';
import { keccak_256 } from '@noble/hashes/sha3.js';
import { bytesToHex, ensureHexPrefix } from './encoding.js';

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

  /**
   * Whether `publicKeyHex` decodes to a valid secp256k1 curve point (compressed
   * 33-byte or uncompressed 65-byte). Unlike {@link EcdsaFormat.validatePublicKeyHex}
   * this rejects well-formed-length but off-curve or bad-prefix keys, and does so
   * without the Miden WASM SDK.
   */
  static isValidPublicKeyPoint(publicKeyHex: string): boolean {
    try {
      secp256k1.ProjectivePoint.fromHex(ensureHexPrefix(publicKeyHex).slice(2));
      return true;
    } catch {
      return false;
    }
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

  /**
   * Recover the compressed (33-byte) secp256k1 public key that produced an
   * `ecdsa_k256_keccak` signature over `messageBytes`.
   *
   * `messageBytes` is the raw word/message bytes (the digest is `keccak256` of
   * these, matching the transaction kernel and {@link EcdsaFormat.keccakDigestHex}).
   * `signature` must be the 65-byte `r||s||v` form; the recovery byte may be the
   * canonical `0/1` or the Ethereum-style `27/28`.
   */
  static recoverCompressedPublicKeyHex(messageBytes: Uint8Array, signature: Uint8Array): string {
    if (signature.length !== ECDSA_SIGNATURE_WITH_RECOVERY_BYTE_LENGTH) {
      throw new Error(
        `ECDSA public-key recovery requires a ${ECDSA_SIGNATURE_WITH_RECOVERY_BYTE_LENGTH}-byte signature (r||s||v), got ${signature.length}`,
      );
    }

    let recoveryBit = signature[ECDSA_SIGNATURE_BYTE_LENGTH];
    if (recoveryBit === 27 || recoveryBit === 28) {
      recoveryBit -= 27;
    }
    if (recoveryBit !== 0 && recoveryBit !== 1) {
      throw new Error(
        `ECDSA recovery byte must be 0/1 (or 27/28), got ${signature[ECDSA_SIGNATURE_BYTE_LENGTH]}`,
      );
    }

    const compact = signature.slice(0, ECDSA_SIGNATURE_BYTE_LENGTH);
    const msgHash = keccak_256(messageBytes);
    const recovered = secp256k1.Signature.fromCompact(compact)
      .addRecoveryBit(recoveryBit)
      .recoverPublicKey(msgHash);
    return bytesToHex(recovered.toRawBytes(true));
  }
}
