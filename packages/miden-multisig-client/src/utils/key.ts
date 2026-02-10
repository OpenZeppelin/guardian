import { Word } from '@miden-sdk/miden-sdk';
import type { SignatureScheme } from '../types.js';
import { bytesToHex } from './encoding.js';
import { tryComputeCommitmentHex } from './signature.js';

export class PublicKeyFormat {
  static parse(publicKey: Uint8Array): {
    scheme: SignatureScheme;
    publicKeyHex: string;
    commitment: string | null;
  } {
    if (publicKey.length === 32) {
      const commitmentHex = PublicKeyFormat.wordBytesToHex(publicKey);
      return { scheme: 'falcon', publicKeyHex: commitmentHex, commitment: commitmentHex };
    }

    const firstByte = publicKey[0];

    if (firstByte === 0x00 && publicKey.length > 100) {
      const hex = bytesToHex(publicKey.slice(1));
      return { scheme: 'falcon', publicKeyHex: hex, commitment: tryComputeCommitmentHex(hex, 'falcon') };
    }

    if (firstByte === 0x01 && publicKey.length === 34) {
      const hex = bytesToHex(publicKey.slice(1));
      return { scheme: 'ecdsa', publicKeyHex: hex, commitment: tryComputeCommitmentHex(hex, 'ecdsa') };
    }

    if (publicKey.length === 33 || publicKey.length === 65) {
      const hex = bytesToHex(publicKey);
      return { scheme: 'ecdsa', publicKeyHex: hex, commitment: tryComputeCommitmentHex(hex, 'ecdsa') };
    }

    const hex = bytesToHex(publicKey);
    return { scheme: 'falcon', publicKeyHex: hex, commitment: tryComputeCommitmentHex(hex, 'falcon') };
  }

  static wordBytesToHex(bytes: Uint8Array): string {
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const u64s = new BigUint64Array(4);
    for (let i = 0; i < 4; i++) {
      u64s[i] = view.getBigUint64(i * 8, true);
    }
    const word = new Word(u64s);
    const hex = word.toHex();
    const clean = hex.replace(/^0x/i, '').toLowerCase().padStart(64, '0');
    return `0x${clean}`;
  }
}
