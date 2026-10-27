import { execFile } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

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
import {
  resolveTargetSigners,
  SignerSchemeMismatchError,
  targetSignerSchemes,
} from '../src/account/signers.js';
import { AccountInspector } from '../src/inspector.js';
import { ProposalMetadataCodec } from '../src/proposal/metadata.js';
import {
  buildUpdateGuardianTransactionRequest,
  buildUpdateSignersTransactionRequest,
  executeForSummaryAtTip,
  requestBoundBlockNum,
} from '../src/transaction.js';
import type { MultisigConfig, SignatureScheme, SignerSpec } from '../src/types.js';
import type { UpdateSignersProposalMetadata } from '../src/types/proposal.js';
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

// `cargo run` compiles the example first when the target dir does not hold it yet: CI prebuilds
// it, but a cold local run takes tens of seconds, far past the default per-test budget.
const FIXTURE_BUILD_TIMEOUT_MS = 600_000;

async function loadFixture(): Promise<MixedSchemeAccountFixture> {
  const repoRoot = fileURLToPath(new URL('../../../', import.meta.url));
  const { stdout } = await promisify(execFile)(
    'cargo',
    ['run', '--quiet', '--example', 'mixed_scheme_account', '-p', 'miden-multisig-client'],
    { cwd: repoRoot, encoding: 'utf8' },
  );
  return JSON.parse(stdout) as MixedSchemeAccountFixture;
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
    let fixture: MixedSchemeAccountFixture;

    beforeAll(async () => {
      fixture = await loadFixture();
    }, FIXTURE_BUILD_TIMEOUT_MS);

    it('reads every approver scheme and the guardian scheme back', () => {
      const account = Account.deserialize(hexToBytes(fixture.account_hex));

      const detected = AccountInspector.fromAccount(account);

      expect(detected.signers).toEqual(fixture.signers);
      expect(detected.signerCommitments).toEqual(fixture.signers.map((s) => s.commitment));
      expect(detected.guardianScheme).toBe(fixture.guardian_scheme);
    });

    it('writes the same code and storage as miden-standards for the same configuration', async () => {
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
      const boundBlockNum = requestBoundBlockNum(unsigned.request);
      const summary = await executeForSummaryAtTip(client, accountId, unsigned.request);
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
        boundBlockNum,
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
    }, 120_000);

    async function storedSigners(): Promise<SignerSpec[]> {
      const account = await client.accounts.get(accountId);
      if (!account) {
        throw new Error('account not found in the store');
      }
      return AccountInspector.fromAccount(account).signers;
    }

    async function summaryCommitment(request: Parameters<typeof executeForSummaryAtTip>[2]): Promise<string> {
      return (await executeForSummaryAtTip(client, accountId, request)).toCommitment().toHex();
    }

    // Issue #576: what a Falcon proposer pushes and an ECDSA cosigner rebuilds
    // from GUARDIAN's copy of the metadata must be the same transaction.
    it('rebuilds a mixed signer update from its wire metadata to the summary its proposer signed', async () => {
      const current = await storedSigners();
      const added: SignerSpec = {
        commitment: tryComputeEcdsaCommitmentHex(
          bytesToHex(secp256k1.getPublicKey(new Uint8Array(32).fill(4), true)),
        ) as string,
        scheme: 'ecdsa',
      };
      const targets = [...current, added];
      const names = targetSignerSchemes(targets, 'falcon', current);
      expect(names).toEqual(['falcon', 'ecdsa', 'ecdsa']);

      const proposerSalt = '0x' + '44'.repeat(32);
      const proposed = await buildUpdateSignersTransactionRequest(client, 2, targets, {
        accountId,
        salt: Word.fromHex(proposerSalt),
        signatureScheme: 'falcon',
      });
      const boundBlockNum = requestBoundBlockNum(proposed.request);
      const signedCommitment = await summaryCommitment(proposed.request);

      const metadata: UpdateSignersProposalMetadata = {
        proposalType: 'add_signer',
        description: '',
        targetThreshold: 2,
        targetSignerCommitments: targets.map((target) => target.commitment),
        targetSignerSchemes: names,
      };
      const served = ProposalMetadataCodec.fromGuardian(
        JSON.parse(JSON.stringify(ProposalMetadataCodec.toGuardian(metadata))),
      ) as UpdateSignersProposalMetadata;

      const cosignerTargets = resolveTargetSigners(
        served.targetSignerCommitments,
        served.targetSignerSchemes,
        await storedSigners(),
      );
      expect(cosignerTargets).toEqual(targets);
      const rebuilt = await buildUpdateSignersTransactionRequest(client, 2, cosignerTargets, {
        accountId,
        salt: Word.fromHex(proposerSalt),
        boundBlockNum,
        signatureScheme: 'ecdsa',
      });
      expect(await summaryCommitment(rebuilt.request)).toBe(signedCommitment);

      // Without the names the cosigner would have to read every target under one
      // scheme, as SDKs before #576 did; that is refused, since it rewrites an approver.
      expect(() =>
        resolveTargetSigners(served.targetSignerCommitments, undefined, current),
      ).toThrow(SignerSchemeMismatchError);
      // And building it that way is another transaction.
      const stamped = await buildUpdateSignersTransactionRequest(
        client,
        2,
        served.targetSignerCommitments,
        { accountId, salt: Word.fromHex(proposerSalt), boundBlockNum, signatureScheme: 'falcon' },
      );
      expect(await summaryCommitment(stamped.request)).not.toBe(signedCommitment);
    }, 120_000);

    // Issue #576: a switch keeps the account's GUARDIAN scheme whatever the
    // proposer's is, so the new GUARDIAN's ECDSA key verifies its acks.
    it("rotates the GUARDIAN under the account's GUARDIAN scheme", async () => {
      const newGuardianKey = new Uint8Array(32).fill(5);
      const newGuardianCommitment = tryComputeEcdsaCommitmentHex(
        bytesToHex(secp256k1.getPublicKey(newGuardianKey, true)),
      ) as string;
      const requestOptions = {
        accountId,
        salt: Word.fromHex('0x' + '55'.repeat(32)),
        signatureScheme: 'ecdsa' as const,
      };
      const unsigned = await buildUpdateGuardianTransactionRequest(
        client,
        newGuardianCommitment,
        requestOptions,
      );
      const boundBlockNum = requestBoundBlockNum(unsigned.request);
      const commitmentHex = await summaryCommitment(unsigned.request);
      const message = () => Word.fromHex(commitmentHex);
      const advice = new AdviceMap();
      for (const entry of [
        buildSignatureAdviceEntry(Word.fromHex(falconCommitment), message(), falconKey.sign(message())),
        buildSignatureAdviceEntry(Word.fromHex(ecdsaCommitment), message(), ecdsaSignature(ecdsaKey, message())),
        buildSignatureAdviceEntry(Word.fromHex(guardianCommitment), message(), ecdsaSignature(guardianKey, message())),
      ]) {
        advice.insert(entry.key, new FeltArray(entry.values));
      }
      const signed = await buildUpdateGuardianTransactionRequest(client, newGuardianCommitment, {
        ...requestOptions,
        salt: Word.fromHex('0x' + '55'.repeat(32)),
        boundBlockNum,
        signatureAdviceMap: advice,
      });

      const execution = await client.transactions.executeRequest(accountId, signed.request);
      await (await (await execution.prove()).submit()).apply();

      const updated = await client.accounts.get(accountId);
      if (!updated) {
        throw new Error('updated account not found in the store');
      }
      const detected = AccountInspector.fromAccount(updated);
      expect(detected.guardianCommitment).toBe(newGuardianCommitment.toLowerCase());
      expect(detected.guardianScheme).toBe('ecdsa');
      expect(detected.signers).toEqual(signers.map((s) => ({ ...s, commitment: s.commitment.toLowerCase() })));
    }, 120_000);
  });
});
