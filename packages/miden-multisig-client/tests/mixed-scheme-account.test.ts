import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import {
  Account,
  AccountBuilder,
  AccountStorageMode,
  AdviceMap,
  AuthSecretKey,
  FeltArray,
  MidenClient,
  Signature,
  Word,
} from '@miden-sdk/miden-sdk';
import { secp256k1 } from '@noble/curves/secp256k1';
import { keccak_256 } from '@noble/hashes/sha3.js';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import {
  buildGuardedMultisigComponentFromLibrary,
  createMultisigAccount,
} from '../src/account/builder.js';
import { AccountInspector } from '../src/inspector.js';
import { buildUpdateSignersTransactionRequest, executeForSummary } from '../src/transaction.js';
import type { MultisigConfig, SignatureScheme, SignerSpec } from '../src/types.js';
import { bytesToHex, hexToBytes } from '../src/utils/encoding.js';
import {
  buildSignatureAdviceEntry,
  signatureHexToBytes,
  tryComputeEcdsaCommitmentHex,
} from '../src/utils/signature.js';
import { wordToBytes } from '../src/utils/word.js';

interface MixedSchemeAccountFixture {
  account_hex: string;
  seed_hex: string;
  threshold: number;
  signers: Array<{ commitment: string; scheme: SignatureScheme }>;
  guardian_commitment: string;
  guardian_scheme: SignatureScheme;
}

let cachedFixture: MixedSchemeAccountFixture | null = null;

function loadFixture(): MixedSchemeAccountFixture {
  if (cachedFixture) {
    return cachedFixture;
  }
  const repoRoot = fileURLToPath(new URL('../../../', import.meta.url));
  const output = execFileSync(
    'cargo',
    ['run', '--quiet', '--example', 'mixed_scheme_account', '-p', 'miden-multisig-client'],
    { cwd: repoRoot, encoding: 'utf8' },
  );
  cachedFixture = JSON.parse(output) as MixedSchemeAccountFixture;
  return cachedFixture;
}

function fixtureConfig(fixture: MixedSchemeAccountFixture): MultisigConfig {
  return {
    threshold: fixture.threshold,
    signerCommitments: fixture.signers,
    guardianCommitment: fixture.guardian_commitment,
    signatureScheme: fixture.guardian_scheme,
    seed: hexToBytes(fixture.seed_hex),
  };
}

function buildAccount(config: MultisigConfig, seed: Uint8Array): Account {
  return new AccountBuilder(seed)
    .storageMode(AccountStorageMode.private())
    .withAuthComponent(buildGuardedMultisigComponentFromLibrary(config))
    .withBasicWalletComponent()
    .buildWithoutSchemaCommitment().account;
}

/**
 * Per-approver signature schemes (issue #539), checked against `miden-standards` itself: the
 * Rust fixture builds the account from the upstream `AuthGuardedMultisig` with one Falcon and one
 * ECDSA approver mix, and the VM runs the auth procedure over a mixed set.
 */
describe('mixed-scheme approver sets', () => {
  describe('against a Rust-built mixed-scheme account', () => {
    it('reads every approver scheme and the guardian scheme back', () => {
      const fixture = loadFixture();
      const account = Account.deserialize(hexToBytes(fixture.account_hex));

      const detected = AccountInspector.fromAccount(account);

      expect(detected.signers).toEqual(fixture.signers);
      expect(detected.signerCommitments).toEqual(fixture.signers.map((s) => s.commitment));
      expect(detected.guardianScheme).toBe(fixture.guardian_scheme);
    });

    it('writes the same code and storage as miden-standards for the same configuration', async () => {
      const fixture = loadFixture();
      const rust = Account.deserialize(hexToBytes(fixture.account_hex));
      const client = await MidenClient.createMock();
      try {
        const { account } = await createMultisigAccount(client, fixtureConfig(fixture));

        expect(account.code().commitment().toHex()).toBe(rust.code().commitment().toHex());
        expect(account.storage().commitment().toHex()).toBe(rust.storage().commitment().toHex());
      } finally {
        client.terminate();
      }
    });
  });

  it.each(['falcon', 'ecdsa'] as const)(
    'builds the same %s single-scheme account through the library path as through the SDK',
    async (scheme) => {
      const seed = new Uint8Array(32).fill(5);
      const config: MultisigConfig = {
        threshold: 2,
        signerCommitments: ['0x' + '01'.repeat(32), '0x' + '02'.repeat(32)],
        guardianCommitment: '0x' + '09'.repeat(32),
        signatureScheme: scheme,
        procedureThresholds: [{ procedure: 'receive_asset', threshold: 1 }],
        seed,
      };
      const client = await MidenClient.createMock();
      try {
        const { account: viaSdk } = await createMultisigAccount(client, config);
        const viaLibrary = buildAccount(config, seed);

        expect(viaLibrary.id().toString()).toBe(viaSdk.id().toString());
        expect(viaLibrary.to_commitment().toHex()).toBe(viaSdk.to_commitment().toHex());
      } finally {
        client.terminate();
      }
    },
  );

  describe('update_signers_and_threshold on the mock chain', () => {
    const SALT_HEX = '0x' + '33'.repeat(32);
    const falconKey = AuthSecretKey.rpoFalconWithRNG(new Uint8Array(32).fill(1));
    const ecdsaKey = new Uint8Array(32).fill(2);
    const guardianKey = new Uint8Array(32).fill(3);
    const ecdsaPublicKey = bytesToHex(secp256k1.getPublicKey(ecdsaKey, true));
    const ecdsaCommitment = tryComputeEcdsaCommitmentHex(ecdsaPublicKey) as string;
    const guardianCommitment = tryComputeEcdsaCommitmentHex(
      bytesToHex(secp256k1.getPublicKey(guardianKey, true)),
    ) as string;
    const falconCommitment = falconKey.getPublicKeyAsWord().toHex();
    const signers: SignerSpec[] = [
      { commitment: falconCommitment, scheme: 'falcon' },
      { commitment: ecdsaCommitment, scheme: 'ecdsa' },
    ];

    let client: MidenClient;
    let accountId: string;

    beforeAll(async () => {
      client = await MidenClient.createMock();
      const { account } = await createMultisigAccount(client, {
        threshold: 2,
        signerCommitments: signers,
        guardianCommitment,
        signatureScheme: 'ecdsa',
        seed: new Uint8Array(32).fill(11),
      });
      accountId = account.id().toString();
    });

    afterAll(() => {
      client?.terminate();
    });

    function ecdsaSignature(key: Uint8Array, message: Word): Signature {
      const signature = secp256k1.sign(keccak_256(wordToBytes(message)), key);
      return Signature.deserialize(
        signatureHexToBytes(
          bytesToHex(new Uint8Array([...signature.toCompactRawBytes(), signature.recovery])),
          'ecdsa',
        ),
      );
    }

    it('accepts a Falcon and an ECDSA approver and keeps each scheme in the new config', async () => {
      const requestOptions = {
        accountId,
        salt: Word.fromHex(SALT_HEX),
        signatureScheme: 'ecdsa' as const,
      };
      const unsigned = await buildUpdateSignersTransactionRequest(client, 1, signers, requestOptions);
      const { summary, anchor } = await executeForSummary(client, accountId, unsigned.request);
      try {
        const commitmentHex = summary.toCommitment().toHex();
        const message = () => Word.fromHex(commitmentHex);
        const entries = [
          buildSignatureAdviceEntry(
            Word.fromHex(falconCommitment),
            message(),
            falconKey.sign(message()),
          ),
          buildSignatureAdviceEntry(
            Word.fromHex(ecdsaCommitment),
            message(),
            ecdsaSignature(ecdsaKey, message()),
          ),
          buildSignatureAdviceEntry(
            Word.fromHex(guardianCommitment),
            message(),
            ecdsaSignature(guardianKey, message()),
          ),
        ];
        const advice = new AdviceMap();
        for (const entry of entries) {
          advice.insert(entry.key, new FeltArray(entry.values));
        }
        const signed = await buildUpdateSignersTransactionRequest(client, 1, signers, {
          ...requestOptions,
          boundBlockNum: anchor.blockNum(),
          signatureAdviceMap: advice,
        });

        const execution = await client.transactions.executeRequest(accountId, signed.request);
        const submission = await (await execution.prove()).submit();
        await submission.apply();

        const updated = await client.accounts.get(accountId);
        if (!updated) {
          throw new Error('updated account not found in the store');
        }
        const detected = AccountInspector.fromAccount(updated);
        expect(detected.threshold).toBe(1);
        expect(detected.signers).toEqual(signers.map((s) => ({ ...s, commitment: s.commitment.toLowerCase() })));
      } finally {
        anchor.free();
      }
    }, 120_000);
  });
});
