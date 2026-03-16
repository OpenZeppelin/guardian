import { AuthSecretKey } from '@miden-sdk/miden-sdk';
import type { Signer, SignatureScheme } from '../types.js';
import { bytesToHex } from '../utils/encoding.js';
import { AuthDigest } from '../utils/digest.js';

export class FalconSigner implements Signer {
  readonly commitment: string;
  readonly publicKey: string;
  readonly scheme: SignatureScheme = 'falcon';
  private readonly secretKey: AuthSecretKey;

  constructor(secretKey: AuthSecretKey) {
    this.secretKey = secretKey;
    const pubKey = secretKey.publicKey();
    this.commitment = pubKey.toCommitment().toHex();
    const serialized = pubKey.serialize();
    const falconPubKey = serialized.slice(1);
    this.publicKey = bytesToHex(falconPubKey);
  }

  async signAccountIdWithTimestamp(accountId: string, timestamp: number): Promise<string> {
    const digest = AuthDigest.fromAccountIdWithTimestamp(accountId, timestamp);
    const signature = this.secretKey.sign(digest);
    const signatureBytes = signature.serialize();
    const falconSignature = signatureBytes.slice(1);
    return bytesToHex(falconSignature);
  }

  async signCommitment(commitmentHex: string): Promise<string> {
    const word = AuthDigest.fromCommitmentHex(commitmentHex);
    const signature = this.secretKey.sign(word);
    const signatureBytes = signature.serialize();
    const falconSignature = signatureBytes.slice(1);
    return bytesToHex(falconSignature);
  }
}
