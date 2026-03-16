import type { Signer, SignatureScheme } from '../types.js';
import { AuthDigest } from '../utils/digest.js';
import { EcdsaFormat } from '../utils/ecdsa.js';
import { hexToBytes, uint8ArrayToBase64 } from '../utils/encoding.js';
import { wordToBytes } from '../utils/word.js';

export interface ParaSigningContext {
  signMessage(params: { walletId: string; messageBase64: string }): Promise<{ signature?: string }>;
}

export class ParaSigner implements Signer {
  readonly commitment: string;
  readonly publicKey: string;
  readonly scheme: SignatureScheme = 'ecdsa';
  private readonly para: ParaSigningContext;
  private readonly walletId: string;

  constructor(
    para: ParaSigningContext,
    walletId: string,
    commitment: string,
    publicKey: string,
  ) {
    if (!EcdsaFormat.validatePublicKeyHex(publicKey)) {
      throw new Error('Invalid ECDSA public key for ParaSigner');
    }
    this.para = para;
    this.walletId = walletId;
    this.commitment = commitment;
    this.publicKey = EcdsaFormat.compressPublicKey(publicKey);
  }

  async signAccountIdWithTimestamp(accountId: string, timestamp: number): Promise<string> {
    const digest = AuthDigest.fromAccountIdWithTimestamp(accountId, timestamp);
    return this.signWord(digest);
  }

  async signCommitment(commitmentHex: string): Promise<string> {
    const word = AuthDigest.fromCommitmentHex(commitmentHex);
    return this.signWord(word);
  }

  private async signWord(word: { toHex: () => string; toFelts: () => Array<{ asInt: () => bigint }> }): Promise<string> {
    const wordBytes = wordToBytes(word);
    const hashedHex = EcdsaFormat.keccakDigestHex(wordBytes);
    const cleanHex = hashedHex.slice(2);
    const messageBase64 = uint8ArrayToBase64(hexToBytes(cleanHex));

    const res = await this.para.signMessage({
      walletId: this.walletId,
      messageBase64,
    });

    if (!res.signature) {
      throw new Error('Para signing was denied by user');
    }

    return EcdsaFormat.normalizeRecoveryByte(res.signature);
  }
}
