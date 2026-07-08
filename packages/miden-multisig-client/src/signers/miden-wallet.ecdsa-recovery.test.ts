import { describe, expect, it } from 'vitest';
import { secp256k1 } from '@noble/curves/secp256k1';
import { keccak_256 } from '@noble/hashes/sha3.js';

import { MidenWalletSigner, type WalletSigningContext } from './miden-wallet.js';
import { tryComputeEcdsaCommitmentHex } from '../utils/signature.js';
import { bytesToHex, hexToBytes } from '../utils/encoding.js';
import { buildGuardianSignatureFromSigner } from '../multisig/signing.js';

// Real-crypto repro for the reported failure:
//   "assertion failed with error message: invalid public key commitment"
//
// This test uses the real WASM SDK (Poseidon2/PublicKey, via tests/setup-wasm.ts)
// and a real secp256k1 keypair — NOT mocks — so it reproduces the exact kernel
// invariant `Poseidon2(publicKey) == pub_key_commitment` that the transaction
// kernel enforces in `ecdsa_k256_keccak.masm` (`assert_eqw "invalid public key
// commitment"`). A wallet-delegated ECDSA cosigner whose `publicKey` degrades to
// the 32-byte commitment fails that invariant.

// Deterministic key so the repro is reproducible run-to-run.
const PRIVATE_KEY = hexToBytes('0x' + '11'.repeat(32));
const COMPRESSED_PUBKEY_HEX = bytesToHex(secp256k1.getPublicKey(PRIVATE_KEY, true));

/**
 * A wallet that signs like the real Miden Wallet ECDSA path: it keccak256-hashes
 * the word bytes it receives, produces a 65-byte `r||s||v` signature, and prepends
 * a 1-byte scheme tag (which `MidenWalletSigner.signWord` strips via `slice(1)`).
 */
function ecdsaWallet(privateKey: Uint8Array): WalletSigningContext {
  return {
    async signBytes(data: Uint8Array): Promise<Uint8Array> {
      const msgHash = keccak_256(data);
      const sig = secp256k1.sign(msgHash, privateKey);
      const compact = sig.toCompactRawBytes(); // 64 bytes: r || s
      const tagged = new Uint8Array(1 + 65);
      tagged[0] = 1; // scheme tag, dropped by the signer
      tagged.set(compact, 1);
      tagged[1 + 64] = sig.recovery; // recovery byte v
      return tagged;
    },
  };
}

describe('MidenWalletSigner ECDSA public-key resolution', () => {
  it('resolves the real ECDSA public key from its own signature so it hashes to the account commitment', async () => {
    // The account's stored commitment is Poseidon2(pubkey) — exactly what the kernel checks.
    const accountCommitment = tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX);
    expect(accountCommitment).not.toBeNull();

    // Wallet-delegated ECDSA cosigner constructed WITHOUT an explicit public key —
    // the reported failure path, where `publicKey` silently fell back to the commitment.
    const signer = new MidenWalletSigner(ecdsaWallet(PRIVATE_KEY), accountCommitment!, 'ecdsa');

    await signer.signCommitment(accountCommitment!);

    // The resolved public key must be the real 33-byte key, not the 32-byte commitment...
    expect(signer.publicKey).toBe(COMPRESSED_PUBKEY_HEX);
    expect(signer.publicKey).not.toBe(accountCommitment);
    // ...and it must Poseidon2-hash back to the commitment (the kernel's assert_eqw).
    expect(tryComputeEcdsaCommitmentHex(signer.publicKey)).toBe(accountCommitment);
  });

  it('fails closed when the wallet signature recovers a key that does not match the commitment', async () => {
    // The account commitment is for PRIVATE_KEY, but the wallet signs with a
    // different key. Recovery must reject the mismatch rather than trust it.
    const otherKey = hexToBytes('0x' + '22'.repeat(32));
    const accountCommitment = tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX);

    const signer = new MidenWalletSigner(ecdsaWallet(otherKey), accountCommitment!, 'ecdsa');
    await signer.signCommitment(accountCommitment!);

    expect(() => signer.publicKey).toThrow(/does not match the signer commitment/);
  });

  it('accepts an explicitly-provided valid ECDSA public key without needing to sign', () => {
    const accountCommitment = tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX);
    const signer = new MidenWalletSigner(
      ecdsaWallet(PRIVATE_KEY),
      accountCommitment!,
      'ecdsa',
      undefined,
      COMPRESSED_PUBKEY_HEX,
    );
    expect(signer.publicKey).toBe(COMPRESSED_PUBKEY_HEX);
  });

  it('compresses an explicitly-provided 65-byte uncompressed public key', () => {
    const uncompressed = bytesToHex(secp256k1.getPublicKey(PRIVATE_KEY, false)); // 65 bytes
    const accountCommitment = tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX);
    const signer = new MidenWalletSigner(
      ecdsaWallet(PRIVATE_KEY),
      accountCommitment!,
      'ecdsa',
      undefined,
      uncompressed,
    );
    expect(signer.publicKey).toBe(COMPRESSED_PUBKEY_HEX);
  });

  it('resolves the public key through a non-signCommitment path (signLookupMessage)', async () => {
    const accountCommitment = tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX);
    const signer = new MidenWalletSigner(ecdsaWallet(PRIVATE_KEY), accountCommitment!, 'ecdsa');

    await signer.signLookupMessage(accountCommitment!, 1700000000);

    expect(signer.publicKey).toBe(COMPRESSED_PUBKEY_HEX);
  });

  it('builds a guardian ProposalSignature carrying the recovered key (integration via buildGuardianSignatureFromSigner)', async () => {
    const accountCommitment = tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX);
    const signer = new MidenWalletSigner(ecdsaWallet(PRIVATE_KEY), accountCommitment!, 'ecdsa');

    // The exact call the guardian flow makes (multisig/signing.ts): sign the
    // commitment, then read `signer.publicKey` for the ProposalSignature that is sent
    // to the guardian and packed into the transaction advice map. This is the seam
    // where the report observed `publicKey === commitment`.
    const proposalSignature = await buildGuardianSignatureFromSigner(signer, accountCommitment!);

    expect(proposalSignature.scheme).toBe('ecdsa');
    if (proposalSignature.scheme !== 'ecdsa') throw new Error('expected an ecdsa ProposalSignature');
    // The signature reaching the guardian/kernel must carry the real 33-byte key,
    // not the 32-byte commitment — otherwise Poseidon2(publicKey) == commitment fails.
    expect(proposalSignature.publicKey).toBe(COMPRESSED_PUBKEY_HEX);
    expect(proposalSignature.publicKey).not.toBe(accountCommitment);
    expect(tryComputeEcdsaCommitmentHex(proposalSignature.publicKey!)).toBe(accountCommitment);
  });
});
