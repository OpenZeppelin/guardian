import type { Account } from '@miden-sdk/miden-sdk';
import {
  AdviceMap,
  FeltArray,
  MidenClient,
  Signature,
  TransactionRequestBuilder,
  Word,
} from '@miden-sdk/miden-sdk';
import { secp256k1 } from '@noble/curves/secp256k1';
import { keccak_256 } from '@noble/hashes/sha3.js';
import type {
  DeltaProposalRequest,
  DeltaProposalResponse,
  GuardianHttpClient,
  Signer,
} from '@openzeppelin/guardian-client';
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';

import { createMultisigAccount } from '../src/account/builder.js';
import { Multisig } from '../src/multisig.js';
import { BoundBlockMismatchError, BoundBlockNotDeclaredError } from '../src/multisig/authArgErrors.js';
import { computeCommitmentFromTxSummary } from '../src/multisig/helpers.js';
import {
  buildUpdateSignersTransactionRequest,
  executeForSummaryAtTip,
  requestBoundBlockNum,
  summaryApprovalExpirationBlockNum,
  summaryBoundBlockNum,
  summarySalt,
} from '../src/transaction.js';
import type { ExportedProposal } from '../src/types.js';
import { midenTransactionTypedData, typedDataDigest } from '../src/utils/eip712.js';
import { bytesToHex, uint8ArrayToBase64 } from '../src/utils/encoding.js';
import {
  buildEip712SignatureAdviceEntry,
  buildSignatureAdviceEntry,
  signatureHexToBytes,
  tryComputeEcdsaCommitmentHex,
} from '../src/utils/signature.js';
import { wordToBytes } from '../src/utils/word.js';

/**
 * Runs the 0.17 guarded-multisig auth procedure in the real VM against the SDK's
 * mock chain, which is the closest this package gets to a node.
 *
 * The request-level tests prove what the builders attach. This is where the auth
 * procedure has to accept it: `resolve_auth_args` pipes the three-word preimage,
 * the summary comes back with the salt and the approval expiration where the
 * readers expect them, and a cosigner rebuilding at the block the proposer bound
 * reproduces the commitment the proposer signed over, at any later tip.
 */
const SIGNER_COMMITMENT = '0x260a375ca01f1f05cd7bf22298b40c47290fc09f209011d39049b7f2ef61387b';
const NEW_SIGNER_COMMITMENT = '0x' + '11'.repeat(31) + '00';
const GUARDIAN_COMMITMENT = '0xc35d79423c41d46b5289aafef48be2364e9ea494c6b14d6aefad10f1a46e6d7c';
const SALT_HEX = '0x' + '22'.repeat(32);

let client: MidenClient;
let accountId: string;

beforeAll(async () => {
  client = await MidenClient.createMock();
  const { account } = await createMultisigAccount(client, {
    threshold: 1,
    signerCommitments: [SIGNER_COMMITMENT],
    guardianCommitment: GUARDIAN_COMMITMENT,
    seed: new Uint8Array(32).fill(9),
  });
  accountId = account.id().toString();
});

afterAll(() => {
  client?.terminate();
});

function buildRequest(options: { boundBlockNum?: number; approvalExpirationDelta?: number } = {}) {
  return buildUpdateSignersTransactionRequest(client, 1, [SIGNER_COMMITMENT, NEW_SIGNER_COMMITMENT], {
    accountId,
    salt: Word.fromHex(SALT_HEX),
    ...options,
  });
}

describe('guarded multisig auth procedure on the mock chain', () => {
  it('executes mixed raw and TypeScript-generated EIP-712 advice', async () => {
    const rawKey = new Uint8Array(32).fill(7);
    const eip712Key = new Uint8Array(32).fill(8);
    const guardianKey = new Uint8Array(32).fill(9);
    const keys = [rawKey, eip712Key, guardianKey];
    const publicKeys = keys.map(key => bytesToHex(secp256k1.getPublicKey(key, true)));
    const commitments = publicKeys.map(key => tryComputeEcdsaCommitmentHex(key));
    if (commitments.some(commitment => !commitment)) {
      throw new Error('Could not derive ECDSA commitments');
    }
    const [rawCommitment, eip712Commitment, guardianCommitment] = commitments as string[];
    const mockClient = await MidenClient.createMock();
    try {
      const { account } = await createMultisigAccount(mockClient, {
        threshold: 2,
        signerCommitments: [rawCommitment, eip712Commitment],
        guardianCommitment,
        signatureScheme: 'ecdsa',
        seed: new Uint8Array(32).fill(10),
      });
      const id = account.id().toString();
      const requestOptions = {
        accountId: id,
        salt: Word.fromHex(SALT_HEX),
        signatureScheme: 'ecdsa' as const,
      };
      const unsigned = await buildUpdateSignersTransactionRequest(
        mockClient, 1, [rawCommitment, eip712Commitment], requestOptions,
      );
      const boundBlockNum = requestBoundBlockNum(unsigned.request);
      const summary = await executeForSummaryAtTip(mockClient, id, unsigned.request);
      const commitment = summary.toCommitment();
      const commitmentHex = commitment.toHex();
      const rawDigest = keccak_256(wordToBytes(Word.fromHex(commitmentHex)));
      const rawSignature = secp256k1.sign(rawDigest, rawKey);
      const guardianSignature = secp256k1.sign(rawDigest, guardianKey);
      const eip712Signature = secp256k1.sign(
        typedDataDigest(midenTransactionTypedData(wordToBytes(Word.fromHex(commitmentHex)))), eip712Key,
      );
      const rawEntry = buildSignatureAdviceEntry(
        Word.fromHex(rawCommitment), Word.fromHex(commitmentHex),
        Signature.deserialize(signatureHexToBytes(bytesToHex(new Uint8Array([
          ...rawSignature.toCompactRawBytes(), rawSignature.recovery,
        ])), 'ecdsa')),
      );
      const eip712Entry = buildEip712SignatureAdviceEntry(
        Word.fromHex(eip712Commitment), Word.fromHex(commitmentHex),
        bytesToHex(new Uint8Array([...eip712Signature.toCompactRawBytes(), eip712Signature.recovery])),
        publicKeys[1],
      );
      const guardianEntry = buildSignatureAdviceEntry(
        Word.fromHex(guardianCommitment), Word.fromHex(commitmentHex),
        Signature.deserialize(signatureHexToBytes(bytesToHex(new Uint8Array([
          ...guardianSignature.toCompactRawBytes(), guardianSignature.recovery,
        ])), 'ecdsa')),
      );
      const advice = new AdviceMap();
      for (const entry of [rawEntry, eip712Entry, guardianEntry]) {
        advice.insert(entry.key, new FeltArray(entry.values));
      }
      const signed = await buildUpdateSignersTransactionRequest(
        mockClient, 1, [rawCommitment, eip712Commitment], {
          ...requestOptions,
          boundBlockNum,
          signatureAdviceMap: advice,
        },
      );
      const execution = await mockClient.transactions.executeRequest(id, signed.request);
      expect(execution.result).toBeDefined();
    } finally {
      mockClient.terminate();
    }
  });

  it('accepts the auth args and binds the salt into the summary', async () => {
    const { request } = await buildRequest();

    const summary = await executeForSummaryAtTip(client, accountId, request);

    expect(summarySalt(summary).toHex()).toBe(SALT_HEX);
    expect(summaryApprovalExpirationBlockNum(summary)).toBeUndefined();
  });

  it('declares the block its auth args bind', async () => {
    const { request } = await buildRequest();

    expect(request.blockNumbers()).toEqual([requestBoundBlockNum(request)]);
  });

  // Issue #462: a multisig summary binds the block its auth args name, not the
  // block it executes against, so a cosigner reproduces it at its own tip long
  // after the proposal was made.
  it('lets a cosigner reproduce the commitment from the salt and the bound block at a later tip', async () => {
    const proposer = await buildRequest();
    const boundBlockNum = requestBoundBlockNum(proposer.request);
    if (boundBlockNum === undefined) {
      throw new Error('the multisig request must carry auth args');
    }
    const summary = await executeForSummaryAtTip(client, accountId, proposer.request);

    await client.proveBlock();
    await client.proveBlock();
    await client.syncChain();
    expect(await client.getSyncHeight()).toBeGreaterThan(boundBlockNum);

    const rebuilt = await buildRequest({ boundBlockNum });
    const reproduced = await executeForSummaryAtTip(client, accountId, rebuilt.request);

    expect(reproduced.toCommitment().toHex()).toBe(summary.toCommitment().toHex());
  });

  it('reads back from the summary the earlier block a request binds', async () => {
    await client.proveBlock();
    await client.syncChain();
    const boundBlockNum = await client.getSyncHeight();
    await client.proveBlock();
    await client.proveBlock();
    await client.syncChain();
    expect(boundBlockNum).toBeGreaterThan(0);
    expect(await client.getSyncHeight()).toBeGreaterThan(boundBlockNum);

    const { request } = await buildRequest({ boundBlockNum });
    const summary = await executeForSummaryAtTip(client, accountId, request);

    expect(summaryBoundBlockNum(summary)).toBe(boundBlockNum);
  });

  it('refuses at the tip a request that binds a block without declaring it', async () => {
    const { request } = await buildRequest();
    const authArg = request.authArg();
    if (!authArg) {
      throw new Error('the multisig request must carry auth args');
    }
    const undeclared = new TransactionRequestBuilder()
      .withAuthArg(authArg)
      .extendAdviceMap(request.adviceMap())
      .build();

    expect(undeclared.blockNumbers()).toEqual([]);
    await expect(
      executeForSummaryAtTip(client, accountId, undeclared),
    ).rejects.toBeInstanceOf(BoundBlockNotDeclaredError);
  });

  it('binds an approval expiration the proposer asks for', async () => {
    const { request } = await buildRequest({ approvalExpirationDelta: 100 });
    const boundBlockNum = requestBoundBlockNum(request);
    if (boundBlockNum === undefined) {
      throw new Error('the multisig request must carry auth args');
    }

    const summary = await executeForSummaryAtTip(client, accountId, request);

    expect(summaryApprovalExpirationBlockNum(summary)).toBe(boundBlockNum + 100);
    expect(summarySalt(summary).toHex()).toBe(SALT_HEX);
  });

  it('produces a different commitment for a different salt', async () => {
    const first = await buildRequest();
    const second = await buildUpdateSignersTransactionRequest(
      client,
      1,
      [SIGNER_COMMITMENT, NEW_SIGNER_COMMITMENT],
      { accountId, salt: Word.fromHex('0x' + '33'.repeat(32)) },
    );

    const a = await executeForSummaryAtTip(client, accountId, first.request);
    const b = await executeForSummaryAtTip(client, accountId, second.request);

    expect(a.toCommitment().toHex()).not.toBe(b.toCommitment().toHex());
  });

  it('refuses an approval expiration the auth procedure would clamp', async () => {
    await expect(buildRequest({ approvalExpirationDelta: 65_536 })).rejects.toThrow(
      /between 1 and 65535/,
    );
  });
});

/**
 * Creation and verification through `Multisig` on the mock chain, against a
 * GUARDIAN stand-in that answers a push with what was pushed (issue #538).
 */
describe('proposals name the block their summary binds', () => {
  let chain: MidenClient;
  let chainAccount: Account;
  let chainAccountId: string;

  const signer = {
    commitment: SIGNER_COMMITMENT,
    publicKey: '0x' + '00'.repeat(32),
    scheme: 'falcon',
    signAccountIdWithTimestamp: () => '0x',
    signCommitment: () => '0x',
  } as Signer;

  const echoingGuardian = {
    pushDeltaProposal: async (request: DeltaProposalRequest): Promise<DeltaProposalResponse> => ({
      commitment: computeCommitmentFromTxSummary(request.deltaPayload.txSummary.data),
      delta: {
        accountId: request.accountId,
        nonce: request.nonce,
        prevCommitment: '0x' + '00'.repeat(32),
        deltaPayload: request.deltaPayload,
        status: {
          status: 'pending',
          timestamp: '2026-10-09T00:00:00Z',
          proposerId: SIGNER_COMMITMENT,
          cosignerSigs: [],
        },
      },
    }),
  } as unknown as GuardianHttpClient;

  beforeAll(async () => {
    chain = client;
    const { account } = await createMultisigAccount(chain, {
      threshold: 1,
      signerCommitments: [SIGNER_COMMITMENT],
      guardianCommitment: GUARDIAN_COMMITMENT,
      seed: new Uint8Array(32).fill(11),
    });
    chainAccount = account;
    chainAccountId = account.id().toString();
  });

  function multisigOnChain(): Multisig {
    return new Multisig(
      chainAccount,
      { threshold: 1, signerCommitments: [SIGNER_COMMITMENT], guardianCommitment: GUARDIAN_COMMITMENT },
      echoingGuardian,
      signer,
      chain,
      chainAccountId,
      'http://localhost:57291',
    );
  }

  async function advanceAndSync(blocks: number): Promise<void> {
    for (let i = 0; i < blocks; i += 1) {
      await chain.proveBlock();
    }
    await chain.syncChain();
  }

  function signerUpdateRequest() {
    return buildUpdateSignersTransactionRequest(chain, 1, [SIGNER_COMMITMENT, NEW_SIGNER_COMMITMENT], {
      accountId: chainAccountId,
      salt: Word.fromHex(SALT_HEX),
    });
  }

  async function exportedAddSigner(): Promise<{ boundBlockNum: number; exported: ExportedProposal }> {
    await chain.syncChain();
    const boundBlockNum = await chain.getSyncHeight();
    const multisig = multisigOnChain();
    const proposal = await multisig.createAddSignerProposal(NEW_SIGNER_COMMITMENT, { nonce: 1 });
    const exported = JSON.parse(multisig.exportProposalToJson(proposal.id)) as ExportedProposal;
    return { boundBlockNum, exported };
  }

  function importOnAnotherClient(exported: ExportedProposal) {
    return multisigOnChain().importProposal(JSON.stringify(exported));
  }

  it('proposes a custom request bound to a block the proposer has synced past', async () => {
    await chain.syncChain();
    const boundBlockNum = await chain.getSyncHeight();
    const { request } = await signerUpdateRequest();
    await advanceAndSync(3);
    expect(await chain.getSyncHeight()).toBeGreaterThan(boundBlockNum);

    const proposal = await multisigOnChain().createCustomProposal(request.serialize(), 'b2agg', { nonce: 1 });

    expect(proposal.metadata.boundBlockNum).toBe(boundBlockNum);
    expect(proposal.metadata.chainAnchor).toBeUndefined();
  });

  it('writes boundBlockNum, and no chainAnchor, on a built-in proposal', async () => {
    const { boundBlockNum, exported } = await exportedAddSigner();

    expect(exported.metadata.boundBlockNum).toBe(boundBlockNum);
    expect(exported.metadata.chainAnchor).toBeUndefined();
  });

  it('rebuilds a built-in proposal at its boundBlockNum on a client that synced past it', async () => {
    const { exported } = await exportedAddSigner();
    delete exported.metadata.chainAnchor;
    await advanceAndSync(2);

    const imported = await importOnAnotherClient(exported);

    expect(imported.verification).toEqual({ status: 'verified' });
  });

  it("verifies a proposal carrying only a legacy chainAnchor from its summary's block", async () => {
    const { boundBlockNum, exported } = await exportedAddSigner();
    await advanceAndSync(2);
    const { request } = await signerUpdateRequest();
    const anchor = await chain.transactions.captureAnchor(request);
    const legacyAnchor = uint8ArrayToBase64(anchor.serialize());
    const anchorBlockNum = anchor.blockNum();
    anchor.free();
    expect(anchorBlockNum).not.toBe(boundBlockNum);
    delete exported.metadata.boundBlockNum;
    exported.metadata.chainAnchor = legacyAnchor;

    const imported = await importOnAnotherClient(exported);

    expect(imported.verification).toEqual({ status: 'verified' });
    expect(imported.metadata.chainAnchor).toBe(legacyAnchor);
  });

  it('verifies a built-in proposal that names no bound block', async () => {
    const { exported } = await exportedAddSigner();
    delete exported.metadata.boundBlockNum;
    delete exported.metadata.chainAnchor;
    await advanceAndSync(2);

    const imported = await importOnAnotherClient(exported);

    expect(imported.verification).toEqual({ status: 'verified' });
  });

  it('refuses a built-in proposal whose boundBlockNum names another block, before re-executing', async () => {
    const { boundBlockNum, exported } = await exportedAddSigner();
    await advanceAndSync(2);
    exported.metadata.boundBlockNum = boundBlockNum + 1;
    const preview = vi.spyOn(chain.transactions, 'preview');

    try {
      const outcome = await importOnAnotherClient(exported).catch((error: unknown) => error);

      expect(preview).not.toHaveBeenCalled();
      expect(outcome).toBeInstanceOf(BoundBlockMismatchError);
    } finally {
      preview.mockRestore();
    }
  });
});
