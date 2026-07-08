import type { RequestAuthPayload } from '@openzeppelin/guardian-client';
import type { Signer, SignatureScheme } from '../types.js';
import { AuthDigest } from '../utils/digest.js';
import { EcdsaFormat } from '../utils/ecdsa.js';
import { bytesToHex, normalizeHexWord } from '../utils/encoding.js';
import { lookupAuthDigest } from '../lookupAuth.js';
import { tryComputeEcdsaCommitmentHex } from '../utils/signature.js';
import { wordToBytes } from '../utils/word.js';

export interface WalletSigningContext {
  /**
   * Sign `data` with the wallet's key.
   *
   * For an ECDSA (secp256k1/keccak) signer that is constructed WITHOUT an
   * explicit public key and without a `localAuthSigner`, the returned signature
   * MUST carry the recovery byte — i.e. the 65-byte `r||s||v` form (optionally
   * prefixed with a 1-byte scheme tag, which is stripped). The public key is
   * recovered from that signature; a 64-byte signature with no recovery byte
   * cannot be recovered and the signer will fail closed when `publicKey` is read.
   */
  signBytes(data: Uint8Array, kind: 'word' | 'signingInputs'): Promise<Uint8Array>;
}

export class MidenWalletSigner implements Signer {
  readonly commitment: string;
  readonly scheme: SignatureScheme;
  private readonly wallet: WalletSigningContext;
  private readonly localAuthSigner: Signer | null;
  private resolvedPublicKey: string | null;
  private lastSignedMessage: Uint8Array | null = null;
  private lastSignature: Uint8Array | null = null;

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

    if (scheme === 'ecdsa') {
      // An ECDSA signer's public key must be a real 33-/65-byte secp256k1 key —
      // never the 32-byte account commitment. An explicit key is validated and
      // compressed up front (as ParaSigner does). Otherwise it is resolved
      // lazily on first `publicKey` read — from a delegate `localAuthSigner`, or
      // recovered from the signer's own signature (see the `publicKey` getter).
      // We never silently fall back to the commitment, which would otherwise only
      // surface as an opaque kernel assertion ("invalid public key commitment")
      // when the transaction executes.
      if (publicKey != null) {
        if (!EcdsaFormat.validatePublicKeyHex(publicKey)) {
          throw new Error(
            'MidenWalletSigner: invalid ECDSA public key (expected a 33- or 65-byte hex key); ' +
              'refusing to fall back to the commitment.',
          );
        }
        this.resolvedPublicKey = EcdsaFormat.compressPublicKey(publicKey);
      } else {
        this.resolvedPublicKey = null;
      }
    } else {
      // Falcon: the public key IS the commitment word, so the commitment is a
      // legitimate value here. Preserve the prior behaviour.
      this.resolvedPublicKey = publicKey ?? localAuthSigner?.publicKey ?? commitment;
    }
  }

  /**
   * The signer's public key.
   *
   * Returns immediately for Falcon and for ECDSA signers constructed with an
   * explicit key or a delegate `localAuthSigner`. For a wallet-delegated ECDSA
   * signer with no supplied key, the key is recovered from a signature the signer
   * has already produced and is verified against {@link commitment} — the same
   * `Poseidon2(publicKey) == commitment` invariant the transaction kernel
   * enforces. In that case, reading `publicKey` before any signature exists, or
   * when the recovered key does not match the commitment, throws: a loud
   * sign-time failure instead of an opaque kernel assertion at execute time.
   */
  get publicKey(): string {
    if (this.resolvedPublicKey !== null) {
      return this.resolvedPublicKey;
    }

    // ECDSA, no explicit key: prefer a delegate signer's (already real) key.
    // Resolved lazily so we never invoke the delegate's getter at construction.
    if (this.localAuthSigner !== null) {
      this.resolvedPublicKey = this.localAuthSigner.publicKey;
      return this.resolvedPublicKey;
    }

    // Otherwise recover the key from a produced signature and verify it against
    // the commitment before trusting it.
    if (this.lastSignature === null || this.lastSignedMessage === null) {
      throw new Error(
        'MidenWalletSigner: ECDSA public key is not available yet — produce a signature first, ' +
          'or pass `publicKey` to the constructor.',
      );
    }

    const recovered = EcdsaFormat.recoverCompressedPublicKeyHex(
      this.lastSignedMessage,
      this.lastSignature,
    );
    const recoveredCommitment = tryComputeEcdsaCommitmentHex(recovered);
    if (recoveredCommitment === null) {
      throw new Error(
        'MidenWalletSigner: could not compute a commitment for the recovered ECDSA public key ' +
          '(is the Miden SDK initialized?).',
      );
    }
    if (recoveredCommitment !== normalizeHexWord(this.commitment)) {
      throw new Error(
        'MidenWalletSigner: the public key recovered from the wallet signature does not match ' +
          'the signer commitment.',
      );
    }

    this.resolvedPublicKey = recovered;
    return recovered;
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
    if (
      this.scheme === 'ecdsa' &&
      this.resolvedPublicKey === null &&
      this.localAuthSigner === null
    ) {
      // No key supplied and no delegate signer: remember the last (message,
      // signature) so `publicKey` can recover and verify the ECDSA key.
      this.lastSignedMessage = bytes;
      this.lastSignature = rawSignature;
    }
    return bytesToHex(rawSignature);
  }
}
