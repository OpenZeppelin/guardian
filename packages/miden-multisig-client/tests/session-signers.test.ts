import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { AuthSecretKey, PublicKey, Signature, Word } from '@miden-sdk/miden-sdk';
import { secp256k1 } from '@noble/curves/secp256k1';
import { keccak_256 } from '@noble/hashes/sha3.js';
import type { SessionGrantFields } from '@openzeppelin/guardian-client';
import { privateKeyToAccount } from 'viem/accounts';
import { describe, expect, it } from 'vitest';

import { LedgerSigner } from '../src/signers/ledger.js';
import { MidenWalletSigner, type WalletSigningContext } from '../src/signers/miden-wallet.js';
import { ParaSigner, type ParaSigningContext } from '../src/signers/para.js';
import { bytesToHex, hexToBytes } from '../src/utils/encoding.js';
import { tryComputeEcdsaCommitmentHex } from '../src/utils/signature.js';

// Each signer signs the session grant and revoke-all through its real wallet
// interface, with a real key behind a mocked wallet. The signatures are then
// checked against the digests the Rust crate generated
// (crates/shared/tests/fixtures/session_grant_vectors.json), with the same
// verification Guardian runs: Miden's own Falcon and ECDSA verify (the WASM
// build of the crypto the server uses) for raw signatures, and a secp256k1
// prehash check of the EIP-712 digest for typed data.

interface Fixture {
  grants: Array<{
    signer_commitment_hex: string;
    session_public_key_hex: string;
    origin: string;
    issued_at: number;
    expires_at: number;
    guardian_commitment_hex: string;
    network: string;
    expected_digest_hex: string;
    expected_eip712_digest_hex: string;
  }>;
  revoke_alls: Array<{
    signer_commitment_hex: string;
    timestamp_ms: number;
    expected_digest_hex: string;
    expected_eip712_digest_hex: string;
  }>;
}

const fixture: Fixture = JSON.parse(
  readFileSync(
    fileURLToPath(
      new URL(
        '../../../crates/shared/tests/fixtures/session_grant_vectors.json',
        import.meta.url,
      ),
    ),
    'utf8',
  ),
);

const grants = fixture.grants.map((grant) => ({
  fields: {
    signerCommitment: grant.signer_commitment_hex,
    sessionPublicKey: grant.session_public_key_hex,
    origin: grant.origin,
    issuedAt: grant.issued_at,
    expiresAt: grant.expires_at,
    guardianCommitment: grant.guardian_commitment_hex,
    network: grant.network,
  } satisfies SessionGrantFields,
  word: grant.expected_digest_hex,
  eip712: grant.expected_eip712_digest_hex,
}));
const revokeAlls = fixture.revoke_alls.filter((revokeAll) => revokeAll.timestamp_ms >= 0);

it('has grant and revoke-all vectors to sign', () => {
  expect(grants.length).toBeGreaterThan(1);
  expect(revokeAlls.length).toBeGreaterThan(0);
});

const PRIVATE_KEY = hexToBytes('0x' + '42'.repeat(32));
const PUBLIC_KEY = bytesToHex(secp256k1.getPublicKey(PRIVATE_KEY, true));

/** Miden's own verification of a raw signature (hex without the scheme tag). */
function midenVerifies(
  publicKey: PublicKey,
  wordHex: string,
  signatureHex: string,
  schemeTag: number,
): boolean {
  const tagged = new Uint8Array([schemeTag, ...hexToBytes(signatureHex)]);
  return publicKey.verify(Word.fromHex(wordHex), Signature.deserialize(tagged));
}

/** The Miden SDK's own ECDSA public key for `PUBLIC_KEY`. */
function midenEcdsaPublicKey(): PublicKey {
  const secret = AuthSecretKey.ecdsaWithRNG(new Uint8Array(32).fill(1));
  const tag = secret.publicKey().serialize()[0];
  return PublicKey.deserialize(new Uint8Array([tag, ...hexToBytes(PUBLIC_KEY)]));
}

const ECDSA_SIGNATURE_TAG = AuthSecretKey.ecdsaWithRNG(new Uint8Array(32).fill(1))
  .sign(Word.fromHex('0x' + '00'.repeat(32)))
  .serialize()[0];

describe('LedgerSigner (EIP-712) session signatures', () => {
  const account = privateKeyToAccount(`0x${'42'.repeat(32)}`);
  const uncompressed = secp256k1.getPublicKey(PRIVATE_KEY, false);
  const address = bytesToHex(keccak_256(uncompressed.slice(1)).slice(-20));
  // Like a wallet parsing the JSON it receives: uint values arrive as decimal
  // strings.
  const provider = {
    request: async ({ params }: { method: string; params: unknown[] }) => {
      const data = JSON.parse(params[1] as string);
      const fields = data.types[data.primaryType] as Array<{ name: string; type: string }>;
      for (const field of fields) {
        if (field.type.startsWith('uint')) {
          data.message[field.name] = BigInt(data.message[field.name]);
        }
      }
      return account.signTypedData(data);
    },
  };
  const signer = new LedgerSigner(provider, PUBLIC_KEY, address);

  it('signs every fixture grant over the digest Guardian checks', async () => {
    for (const grant of grants) {
      const signature = hexToBytes(await signer.signSessionGrant(grant.fields));
      expect(
        secp256k1.verify(signature.slice(0, 64), hexToBytes(grant.eip712), hexToBytes(PUBLIC_KEY)),
      ).toBe(true);
    }
    const other = hexToBytes(await signer.signSessionGrant(grants[0].fields));
    expect(
      secp256k1.verify(other.slice(0, 64), hexToBytes(grants[1].eip712), hexToBytes(PUBLIC_KEY)),
    ).toBe(false);
  });

  it('signs every fixture revoke-all over the digest Guardian checks', async () => {
    for (const revokeAll of revokeAlls) {
      const signature = hexToBytes(
        await signer.signSessionRevokeAll(revokeAll.signer_commitment_hex, revokeAll.timestamp_ms),
      );
      expect(
        secp256k1.verify(
          signature.slice(0, 64),
          hexToBytes(revokeAll.expected_eip712_digest_hex),
          hexToBytes(PUBLIC_KEY),
        ),
      ).toBe(true);
    }
  });
});

describe('ParaSigner (raw ECDSA) session signatures', () => {
  const para: ParaSigningContext = {
    signMessage: async ({ messageBase64 }) => {
      const digest = Uint8Array.from(atob(messageBase64), (char) => char.charCodeAt(0));
      const signature = secp256k1.sign(digest, PRIVATE_KEY);
      return {
        signature: bytesToHex(
          new Uint8Array([...signature.toCompactRawBytes(), signature.recovery]),
        ),
      };
    },
  };
  const signer = new ParaSigner(para, 'wallet-1', '0x' + '00'.repeat(32), PUBLIC_KEY);
  const publicKey = midenEcdsaPublicKey();

  it('signs every fixture grant so Miden verifies it over the grant digest', async () => {
    for (const grant of grants) {
      const signature = await signer.signSessionGrant(grant.fields);
      expect(midenVerifies(publicKey, grant.word, signature, ECDSA_SIGNATURE_TAG)).toBe(true);
    }
    const other = await signer.signSessionGrant(grants[0].fields);
    expect(midenVerifies(publicKey, grants[1].word, other, ECDSA_SIGNATURE_TAG)).toBe(false);
  });

  it('signs every fixture revoke-all so Miden verifies it', async () => {
    for (const revokeAll of revokeAlls) {
      const signature = await signer.signSessionRevokeAll(
        revokeAll.signer_commitment_hex,
        revokeAll.timestamp_ms,
      );
      expect(
        midenVerifies(publicKey, revokeAll.expected_digest_hex, signature, ECDSA_SIGNATURE_TAG),
      ).toBe(true);
    }
  });
});

describe('MidenWalletSigner session signatures', () => {
  /** A wallet that signs the word it receives with `secret`, as the extension does. */
  function wallet(secret: AuthSecretKey): WalletSigningContext {
    return {
      async signBytes(data: Uint8Array): Promise<Uint8Array> {
        const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
        const word = new Word(
          BigUint64Array.from([0, 1, 2, 3], (i) => view.getBigUint64(i * 8, true)),
        );
        return secret.sign(word).serialize();
      },
    };
  }

  it('signs grants and revoke-alls with a Falcon key so Miden verifies them', async () => {
    const secret = AuthSecretKey.rpoFalconWithRNG(new Uint8Array(32).fill(9));
    const falconTag = secret.sign(Word.fromHex('0x' + '00'.repeat(32))).serialize()[0];
    const commitment = secret.publicKey().toCommitment().toHex();
    const signer = new MidenWalletSigner(wallet(secret), commitment, 'falcon');

    for (const grant of grants) {
      const signature = await signer.signSessionGrant(grant.fields);
      expect(midenVerifies(secret.publicKey(), grant.word, signature, falconTag)).toBe(true);
    }
    const other = await signer.signSessionGrant(grants[0].fields);
    expect(midenVerifies(secret.publicKey(), grants[1].word, other, falconTag)).toBe(false);
    for (const revokeAll of revokeAlls) {
      const signature = await signer.signSessionRevokeAll(
        revokeAll.signer_commitment_hex,
        revokeAll.timestamp_ms,
      );
      expect(
        midenVerifies(secret.publicKey(), revokeAll.expected_digest_hex, signature, falconTag),
      ).toBe(true);
    }
  });

  it('signs grants and revoke-alls with an ECDSA key so Miden verifies them', async () => {
    const ecdsaWallet: WalletSigningContext = {
      async signBytes(data: Uint8Array): Promise<Uint8Array> {
        const signature = secp256k1.sign(keccak_256(data), PRIVATE_KEY);
        return new Uint8Array([
          ECDSA_SIGNATURE_TAG,
          ...signature.toCompactRawBytes(),
          signature.recovery,
        ]);
      },
    };
    const commitment = tryComputeEcdsaCommitmentHex(PUBLIC_KEY);
    expect(commitment).toBeTruthy();
    const signer = new MidenWalletSigner(ecdsaWallet, commitment!, 'ecdsa', undefined, PUBLIC_KEY);
    const publicKey = midenEcdsaPublicKey();

    for (const grant of grants) {
      const signature = await signer.signSessionGrant(grant.fields);
      expect(midenVerifies(publicKey, grant.word, signature, ECDSA_SIGNATURE_TAG)).toBe(true);
    }
    for (const revokeAll of revokeAlls) {
      const signature = await signer.signSessionRevokeAll(
        revokeAll.signer_commitment_hex,
        revokeAll.timestamp_ms,
      );
      expect(
        midenVerifies(publicKey, revokeAll.expected_digest_hex, signature, ECDSA_SIGNATURE_TAG),
      ).toBe(true);
    }
  });
});
