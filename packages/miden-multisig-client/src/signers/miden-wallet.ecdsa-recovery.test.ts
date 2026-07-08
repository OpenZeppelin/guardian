import { describe, expect, it } from 'vitest';
import { secp256k1 } from '@noble/curves/secp256k1';
import { keccak_256 } from '@noble/hashes/sha3.js';

import { MidenWalletSigner, type WalletSigningContext } from './miden-wallet.js';
import type { Signer } from '../types.js';
import { tryComputeEcdsaCommitmentHex } from '../utils/signature.js';
import { bytesToHex, hexToBytes } from '../utils/encoding.js';
import { buildGuardianSignatureFromSigner } from '../multisig/signing.js';

// Real-crypto repro for the reported failure:
//   "assertion failed with error message: invalid public key commitment"
//
// These tests use the real WASM SDK (Poseidon2/PublicKey, via tests/setup-wasm.ts)
// and real secp256k1 keypairs — NOT mocks — so they exercise the exact kernel
// invariant `Poseidon2(publicKey) == pub_key_commitment` that the transaction
// kernel enforces in `ecdsa_k256_keccak.masm` (`assert_eqw "invalid public key
// commitment"`). A wallet-delegated ECDSA cosigner whose `publicKey` degrades to
// the 32-byte commitment — or is a valid-but-wrong key — fails that invariant.

// Deterministic keys so the repro is reproducible run-to-run.
const PRIVATE_KEY = hexToBytes('0x' + '11'.repeat(32));
const COMPRESSED_PUBKEY_HEX = bytesToHex(secp256k1.getPublicKey(PRIVATE_KEY, true));

// A second, unrelated key: valid secp256k1 but NOT the one the commitment is for.
const OTHER_PRIVATE_KEY = hexToBytes('0x' + '22'.repeat(32));
const OTHER_COMPRESSED_PUBKEY_HEX = bytesToHex(secp256k1.getPublicKey(OTHER_PRIVATE_KEY, true));

/** Asserts a nullable value is present and returns it narrowed (avoids `!`). */
function expectPresent<T>(value: T | null | undefined): T {
  expect(value ?? null).not.toBeNull();
  return value as T;
}

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

/** Minimal ECDSA delegate (`localAuthSigner`) carrying a fixed public key. */
function ecdsaDelegate(commitment: string, publicKey: string): Signer {
  return {
    commitment,
    publicKey,
    scheme: 'ecdsa',
    signAccountIdWithTimestamp: async () => '0x00',
    signCommitment: async () => '0x00',
  };
}

describe('MidenWalletSigner ECDSA public-key resolution', () => {
  it('resolves the real ECDSA public key from its own signature so it hashes to the account commitment', async () => {
    // The account's stored commitment is Poseidon2(pubkey) — exactly what the kernel checks.
    const accountCommitment = expectPresent(tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX));

    // Wallet-delegated ECDSA cosigner constructed WITHOUT an explicit public key —
    // the reported failure path, where `publicKey` silently fell back to the commitment.
    const signer = new MidenWalletSigner(ecdsaWallet(PRIVATE_KEY), accountCommitment, 'ecdsa');

    await signer.signCommitment(accountCommitment);

    // The resolved public key must be the real 33-byte key, not the 32-byte commitment...
    expect(signer.publicKey).toBe(COMPRESSED_PUBKEY_HEX);
    expect(signer.publicKey).not.toBe(accountCommitment);
    // ...and it must Poseidon2-hash back to the commitment (the kernel's assert_eqw).
    expect(tryComputeEcdsaCommitmentHex(signer.publicKey)).toBe(accountCommitment);
  });

  it('fails closed when the wallet signature recovers a key that does not match the commitment', async () => {
    // The account commitment is for PRIVATE_KEY, but the wallet signs with a
    // different key. Recovery must reject the mismatch rather than trust it.
    const accountCommitment = expectPresent(tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX));

    const signer = new MidenWalletSigner(ecdsaWallet(OTHER_PRIVATE_KEY), accountCommitment, 'ecdsa');
    await signer.signCommitment(accountCommitment);

    expect(() => signer.publicKey).toThrow(/does not match the signer commitment/);
  });

  it('accepts an explicit ECDSA public key whose commitment matches, without needing to sign', () => {
    const accountCommitment = expectPresent(tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX));
    const signer = new MidenWalletSigner(
      ecdsaWallet(PRIVATE_KEY),
      accountCommitment,
      'ecdsa',
      undefined,
      COMPRESSED_PUBKEY_HEX,
    );
    expect(signer.publicKey).toBe(COMPRESSED_PUBKEY_HEX);
    expect(tryComputeEcdsaCommitmentHex(signer.publicKey)).toBe(accountCommitment);
  });

  it('rejects an explicit ECDSA public key whose commitment does not match the signer commitment', () => {
    // A well-formed 33-byte key (passes format validation) that belongs to a
    // different account. It must be rejected at the source, not carried into signing.
    const accountCommitment = expectPresent(tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX));
    const signer = new MidenWalletSigner(
      ecdsaWallet(PRIVATE_KEY),
      accountCommitment,
      'ecdsa',
      undefined,
      OTHER_COMPRESSED_PUBKEY_HEX,
    );
    expect(() => signer.publicKey).toThrow(/does not match the signer commitment/);
  });

  it('rejects an explicit ECDSA key that is a valid length but not a secp256k1 point', () => {
    // 33-byte, correct compressed tag, but x is off-curve — must be rejected at
    // construction, SDK-independently, not silently carried to execute time.
    const accountCommitment = expectPresent(tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX));
    expect(
      () =>
        new MidenWalletSigner(
          ecdsaWallet(PRIVATE_KEY),
          accountCommitment,
          'ecdsa',
          undefined,
          '0x02' + 'ff'.repeat(32),
        ),
    ).toThrow(/not a valid secp256k1 point/);
  });

  it('compresses an explicitly-provided 65-byte uncompressed public key', () => {
    const uncompressed = bytesToHex(secp256k1.getPublicKey(PRIVATE_KEY, false)); // 65 bytes
    const accountCommitment = expectPresent(tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX));
    const signer = new MidenWalletSigner(
      ecdsaWallet(PRIVATE_KEY),
      accountCommitment,
      'ecdsa',
      undefined,
      uncompressed,
    );
    expect(signer.publicKey).toBe(COMPRESSED_PUBKEY_HEX);
  });

  it('accepts a localAuthSigner whose ECDSA public key matches the commitment', () => {
    const accountCommitment = expectPresent(tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX));
    const delegate = ecdsaDelegate(accountCommitment, COMPRESSED_PUBKEY_HEX);
    const signer = new MidenWalletSigner(ecdsaWallet(PRIVATE_KEY), accountCommitment, 'ecdsa', delegate);
    expect(signer.publicKey).toBe(COMPRESSED_PUBKEY_HEX);
    expect(tryComputeEcdsaCommitmentHex(signer.publicKey)).toBe(accountCommitment);
  });

  it('rejects a localAuthSigner whose ECDSA public key does not match the commitment', () => {
    const accountCommitment = expectPresent(tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX));
    const delegate = ecdsaDelegate(accountCommitment, OTHER_COMPRESSED_PUBKEY_HEX);
    const signer = new MidenWalletSigner(ecdsaWallet(PRIVATE_KEY), accountCommitment, 'ecdsa', delegate);
    expect(() => signer.publicKey).toThrow(/does not match the signer commitment/);
  });

  it('rejects a localAuthSigner whose ECDSA key is a valid length but not a secp256k1 point', () => {
    const accountCommitment = expectPresent(tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX));
    const delegate = ecdsaDelegate(accountCommitment, '0x02' + 'ff'.repeat(32));
    const signer = new MidenWalletSigner(ecdsaWallet(PRIVATE_KEY), accountCommitment, 'ecdsa', delegate);
    expect(() => signer.publicKey).toThrow(/not a valid secp256k1 point/);
  });

  it('resolves the public key through a non-signCommitment path (signLookupMessage)', async () => {
    const accountCommitment = expectPresent(tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX));
    const signer = new MidenWalletSigner(ecdsaWallet(PRIVATE_KEY), accountCommitment, 'ecdsa');

    await signer.signLookupMessage(accountCommitment, 1700000000);

    expect(signer.publicKey).toBe(COMPRESSED_PUBKEY_HEX);
  });

  it('builds a guardian ProposalSignature carrying the recovered key (integration via buildGuardianSignatureFromSigner)', async () => {
    const accountCommitment = expectPresent(tryComputeEcdsaCommitmentHex(COMPRESSED_PUBKEY_HEX));
    const signer = new MidenWalletSigner(ecdsaWallet(PRIVATE_KEY), accountCommitment, 'ecdsa');

    // The exact call the guardian flow makes (multisig/signing.ts): sign the
    // commitment, then read `signer.publicKey` for the ProposalSignature that is sent
    // to the guardian and packed into the transaction advice map. This is the seam
    // where the report observed `publicKey === commitment`.
    const proposalSignature = await buildGuardianSignatureFromSigner(signer, accountCommitment);

    expect(proposalSignature.scheme).toBe('ecdsa');
    if (proposalSignature.scheme !== 'ecdsa') throw new Error('expected an ecdsa ProposalSignature');
    // The signature reaching the guardian/kernel must carry the real 33-byte key,
    // not the 32-byte commitment — otherwise Poseidon2(publicKey) == commitment fails.
    const resolvedKey = expectPresent(proposalSignature.publicKey);
    expect(resolvedKey).toBe(COMPRESSED_PUBKEY_HEX);
    expect(resolvedKey).not.toBe(accountCommitment);
    expect(tryComputeEcdsaCommitmentHex(resolvedKey)).toBe(accountCommitment);
  });
});
