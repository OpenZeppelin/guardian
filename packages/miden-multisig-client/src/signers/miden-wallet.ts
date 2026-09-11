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
   * `signBytes` MUST return the signature in the Miden SDK's serialized form: a
   * 1-byte scheme tag followed by the raw signature. The signer always strips that
   * leading tag byte, exactly as the SDK's own `EcdsaSigner`/`FalconSigner` do with
   * `Signature.serialize()`.
   *
   * For an ECDSA (secp256k1/keccak) signer constructed WITHOUT an explicit public
   * key and without a `localAuthSigner`, the raw signature MUST carry the recovery
   * byte — the 65-byte `r||s||v` form (66 bytes including the tag) — so the public
   * key can be recovered from it. A 64-byte signature with no recovery byte cannot
   * be recovered, and the signer fails closed when `publicKey` is read.
   */
  signBytes(data: Uint8Array, kind: 'word' | 'signingInputs'): Promise<Uint8Array>;
}

export class MidenWalletSigner implements Signer {
  readonly commitment: string;
  readonly scheme: SignatureScheme;
  private readonly wallet: WalletSigningContext;
  private readonly localAuthSigner: Signer | null;
  private readonly explicitPublicKey: string | null;
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
      this.explicitPublicKey = publicKey == null ? null : validateEcdsaPublicKeyHex(publicKey);
      this.resolvedPublicKey = null;
    } else {
      this.explicitPublicKey = null;
      this.resolvedPublicKey = publicKey ?? localAuthSigner?.publicKey ?? commitment;
    }
  }

  /**
   * The signer's public key.
   *
   * Returns immediately for Falcon. For ECDSA the key is resolved from one of
   * three sources — an explicit key, a delegate `localAuthSigner`, or recovery
   * from a signature the signer has already produced — and every source is
   * verified against {@link commitment} via the same `Poseidon2(publicKey) ==
   * commitment` invariant the transaction kernel enforces. Reading `publicKey`
   * before any source is available, or when the resolved key does not match the
   * commitment, throws: a loud sign-time failure instead of an opaque kernel
   * assertion ("invalid public key commitment") at execute time.
   */
  get publicKey(): string {
    if (this.resolvedPublicKey !== null) {
      return this.resolvedPublicKey;
    }

    if (this.explicitPublicKey !== null) {
      return this.verifyAndCache(this.explicitPublicKey, false);
    }

    if (this.localAuthSigner !== null) {
      return this.verifyAndCache(validateEcdsaPublicKeyHex(this.localAuthSigner.publicKey), false);
    }

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
    return this.verifyAndCache(recovered, true);
  }

  /**
   * Verify an already point-validated ECDSA public-key candidate against
   * {@link commitment} and cache it on success.
   *
   * A mismatching commitment is always rejected. `requireCommitment` distinguishes
   * the recovery path — where a signature already exists, so the SDK must be
   * initialized and an uncomputable commitment is an error — from the
   * explicit/delegate paths, where a caller may read `publicKey` before the SDK is
   * initialized. In that cold-SDK case the commitment cannot be computed yet, so
   * the candidate is returned WITHOUT being cached and verification is re-attempted
   * on the next read (once the SDK is up, as it always is in the guardian flow).
   */
  private verifyAndCache(candidate: string, requireCommitment: boolean): string {
    const candidateCommitment = tryComputeEcdsaCommitmentHex(candidate);
    if (candidateCommitment === null) {
      if (requireCommitment) {
        throw new Error(
          'MidenWalletSigner: could not compute a commitment for the recovered ECDSA public key ' +
            '(is the Miden SDK initialized?).',
        );
      }
      return candidate;
    }
    if (candidateCommitment !== normalizeHexWord(this.commitment)) {
      throw new Error(
        'MidenWalletSigner: the ECDSA public key does not match the signer commitment.',
      );
    }

    this.resolvedPublicKey = candidate;
    return candidate;
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
      this.explicitPublicKey === null &&
      this.localAuthSigner === null
    ) {
      this.lastSignedMessage = bytes;
      this.lastSignature = rawSignature;
    }
    return bytesToHex(rawSignature);
  }
}

/**
 * Validate an ECDSA public key and return its compressed (33-byte) form.
 *
 * The key must be a 33-/65-byte hex key AND a real secp256k1 curve point; a
 * malformed or off-curve key throws immediately (SDK-independent) rather than
 * degrading to the commitment or surfacing as an opaque kernel assertion at
 * execute time. Mirrors the format handling ParaSigner does for explicit keys.
 */
function validateEcdsaPublicKeyHex(publicKey: string): string {
  if (!EcdsaFormat.validatePublicKeyHex(publicKey)) {
    throw new Error(
      'MidenWalletSigner: invalid ECDSA public key (expected a 33- or 65-byte hex key); ' +
        'refusing to fall back to the commitment.',
    );
  }
  const compressed = EcdsaFormat.compressPublicKey(publicKey);
  if (!EcdsaFormat.isValidPublicKeyPoint(compressed)) {
    throw new Error(
      'MidenWalletSigner: invalid ECDSA public key (not a valid secp256k1 point); ' +
        'refusing to fall back to the commitment.',
    );
  }
  return compressed;
}
