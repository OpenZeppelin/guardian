import { AuthSecretKey } from '@miden-sdk/miden-sdk';
import type { Signer, SignatureScheme } from '../types.js';
import { bytesToHex, normalizeHexWord } from '../utils/encoding.js';
import { AuthDigest } from '../utils/digest.js';

export class EcdsaSigner implements Signer {
  readonly commitment: string;
  readonly publicKey: string;
  readonly scheme: SignatureScheme = 'ecdsa';
  private readonly secretKey: AuthSecretKey;

  constructor(secretKey: AuthSecretKey) {
    this.secretKey = secretKey;
    const pubKey = secretKey.publicKey();
    const serialized = pubKey.serialize();
    this.publicKey = bytesToHex(serialized.slice(1));
    this.commitment = normalizeHexWord(pubKey.toCommitment().toHex());
  }

  async signAccountIdWithTimestamp(accountId: string, timestamp: number): Promise<string> {
    const digest = AuthDigest.fromAccountIdWithTimestamp(accountId, timestamp);
    const signature = this.secretKey.sign(digest);
    const signatureBytes = signature.serialize();
    const ecdsaSignature = signatureBytes.slice(1);
    return bytesToHex(ecdsaSignature);
  }

  async signCommitment(commitmentHex: string): Promise<string> {
    const word = AuthDigest.fromCommitmentHex(commitmentHex);
    const signature = this.secretKey.sign(word);
    const signatureBytes = signature.serialize();
    const ecdsaSignature = signatureBytes.slice(1);
    return bytesToHex(ecdsaSignature);
  }
}
