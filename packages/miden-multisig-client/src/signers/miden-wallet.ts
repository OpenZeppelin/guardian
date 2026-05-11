import type { RequestAuthPayload } from '@openzeppelin/guardian-client';
import type { Signer, SignatureScheme } from '../types.js';
import { AuthDigest } from '../utils/digest.js';
import { bytesToHex } from '../utils/encoding.js';
import { lookupAuthDigest } from '../lookupAuth.js';
import { wordToBytes } from '../utils/word.js';

export interface WalletSigningContext {
  signBytes(data: Uint8Array, kind: 'word' | 'signingInputs'): Promise<Uint8Array>;
}

export class MidenWalletSigner implements Signer {
  readonly commitment: string;
  readonly publicKey: string;
  readonly scheme: SignatureScheme;
  private readonly wallet: WalletSigningContext;
  private readonly localAuthSigner: Signer | null;

  constructor(
    wallet: WalletSigningContext,
    commitment: string,
    scheme: SignatureScheme,
    localAuthSigner?: Signer,
    publicKey?: string,
  ) {
    this.wallet = wallet;
    this.commitment = commitment;
    this.scheme = scheme;
    this.localAuthSigner = localAuthSigner ?? null;
    this.publicKey = publicKey ?? localAuthSigner?.publicKey ?? commitment;
  }

  async signAccountIdWithTimestamp(accountId: string, timestamp: number): Promise<string> {
    if (this.localAuthSigner) {
      return this.localAuthSigner.signAccountIdWithTimestamp(accountId, timestamp);
    }
    const word = AuthDigest.fromAccountIdWithTimestamp(accountId, timestamp);
    return this.signWord(word);
  }

  async signRequest(
    accountId: string,
    timestamp: number,
    requestPayload: RequestAuthPayload,
  ): Promise<string> {
    if (this.localAuthSigner?.signRequest) {
      return this.localAuthSigner.signRequest(accountId, timestamp, requestPayload);
    }

    if (this.scheme === 'falcon') {
      return this.signWord(AuthDigest.fromRequest(accountId, timestamp, requestPayload));
    }
    return this.signWord(AuthDigest.fromRequest(accountId, timestamp, requestPayload));
  }

  async signCommitment(commitmentHex: string): Promise<string> {
    const word = AuthDigest.fromCommitmentHex(commitmentHex);
    return this.signWord(word);
  }

  /**
   * Sign a `LookupAuthMessage` digest for the `/state/lookup` endpoint.
   * Account-less; used directly by `recoverByKey`.
   */
  async signLookupMessage(keyCommitmentHex: string, timestampMs: number): Promise<string> {
    if (this.localAuthSigner?.signLookupMessage) {
      return this.localAuthSigner.signLookupMessage(keyCommitmentHex, timestampMs);
    }
    const digest = lookupAuthDigest(timestampMs, keyCommitmentHex);
    return this.signWord(digest);
  }

  private async signWord(word: { toFelts: () => Array<{ asInt: () => bigint }> }): Promise<string> {
    const bytes = wordToBytes(word);
    const signatureBytes = await this.wallet.signBytes(bytes, 'word');
    const rawSignature = signatureBytes.slice(1);
    return bytesToHex(rawSignature);
  }
}
