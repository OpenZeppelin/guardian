import { AccountId, AdviceMap, FeltArray, MockWebClient, Signature, Word } from '@miden-sdk/miden-sdk';
import { secp256k1 } from '@noble/curves/secp256k1';
import { keccak_256 } from '@noble/hashes/sha3.js';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { createMultisigAccount } from '../src/account/builder.js';
import {
  buildUpdateSignersTransactionRequest,
  executeForSummary,
  executeForSummaryAt,
  requestBoundBlockNum,
  summaryApprovalExpirationBlockNum,
  summarySalt,
} from '../src/transaction.js';
import { midenTransactionTypedData, typedDataDigest } from '../src/utils/eip712.js';
import { bytesToHex } from '../src/utils/encoding.js';
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
 * readers expect them, and a cosigner rebuilding at the proposer's anchor
 * reproduces the commitment the proposer signed over.
 */
const SIGNER_COMMITMENT = '0x260a375ca01f1f05cd7bf22298b40c47290fc09f209011d39049b7f2ef61387b';
const NEW_SIGNER_COMMITMENT = '0x' + '11'.repeat(31) + '00';
const GUARDIAN_COMMITMENT = '0xc35d79423c41d46b5289aafef48be2364e9ea494c6b14d6aefad10f1a46e6d7c';
const SALT_HEX = '0x' + '22'.repeat(32);
const RPC = 'mock';

let client: MockWebClient;
let accountId: string;

beforeAll(async () => {
  client = await MockWebClient.createClient();
  const { account } = await createMultisigAccount(
    client,
    {
      threshold: 1,
      signerCommitments: [SIGNER_COMMITMENT],
      guardianCommitment: GUARDIAN_COMMITMENT,
      seed: new Uint8Array(32).fill(9),
    },
    RPC,
  );
  accountId = account.id().toString();
});

afterAll(() => {
  client?.free?.();
});

function buildRequest(options: { boundBlockNum?: number; approvalExpirationDelta?: number } = {}) {
  return buildUpdateSignersTransactionRequest(client, 1, [SIGNER_COMMITMENT, NEW_SIGNER_COMMITMENT], {
    accountId,
    salt: Word.fromHex(SALT_HEX),
    midenRpcEndpoint: RPC,
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
    const mockClient = await MockWebClient.createClient();
    try {
      const { account } = await createMultisigAccount(mockClient, {
        threshold: 2,
        signerCommitments: [rawCommitment, eip712Commitment],
        guardianCommitment,
        signatureScheme: 'ecdsa',
        seed: new Uint8Array(32).fill(10),
      }, RPC);
      const id = account.id().toString();
      const requestOptions = {
        accountId: id,
        salt: Word.fromHex(SALT_HEX),
        midenRpcEndpoint: RPC,
        signatureScheme: 'ecdsa' as const,
      };
      const unsigned = await buildUpdateSignersTransactionRequest(
        mockClient, 1, [rawCommitment, eip712Commitment], requestOptions,
      );
      const { summary, anchor } = await executeForSummary(mockClient, id, unsigned.request, RPC);
      try {
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
            boundBlockNum: anchor.blockNum(),
            signatureAdviceMap: advice,
          },
        );
        const result = await mockClient.executeTransaction(AccountId.fromHex(id), signed.request);
        expect(result).toBeDefined();
      } finally {
        anchor.free();
      }
    } finally {
      mockClient.free();
    }
  });

  it('accepts the auth args and binds the salt into the summary', async () => {
    const { request } = await buildRequest();

    const { summary, anchor } = await executeForSummary(client, accountId, request, RPC);
    try {
      expect(summarySalt(summary).toHex()).toBe(SALT_HEX);
      expect(summaryApprovalExpirationBlockNum(summary)).toBeUndefined();
      expect(anchor.blockNum()).toBe(requestBoundBlockNum(request));
      expect(summary.blockCommitment().toHex()).toBe(anchor.commitment().toHex());
    } finally {
      anchor.free();
    }
  });

  it('lets a cosigner reproduce the commitment from the salt and the anchor block', async () => {
    const proposer = await buildRequest();
    const { summary, anchor } = await executeForSummary(client, accountId, proposer.request, RPC);
    try {
      const rebuilt = await buildRequest({ boundBlockNum: anchor.blockNum() });
      const reproduced = await executeForSummaryAt(client, accountId, rebuilt.request, anchor, RPC);

      expect(reproduced.toCommitment().toHex()).toBe(summary.toCommitment().toHex());
    } finally {
      anchor.free();
    }
  });

  it('binds an approval expiration the proposer asks for', async () => {
    const { request } = await buildRequest({ approvalExpirationDelta: 100 });

    const { summary, anchor } = await executeForSummary(client, accountId, request, RPC);
    try {
      expect(summaryApprovalExpirationBlockNum(summary)).toBe(anchor.blockNum() + 100);
      expect(summarySalt(summary).toHex()).toBe(SALT_HEX);
    } finally {
      anchor.free();
    }
  });

  it('produces a different commitment for a different salt', async () => {
    const first = await buildRequest();
    const second = await buildUpdateSignersTransactionRequest(
      client,
      1,
      [SIGNER_COMMITMENT, NEW_SIGNER_COMMITMENT],
      { accountId, salt: Word.fromHex('0x' + '33'.repeat(32)), midenRpcEndpoint: RPC },
    );

    const a = await executeForSummary(client, accountId, first.request, RPC);
    const b = await executeForSummary(client, accountId, second.request, RPC);
    try {
      expect(a.summary.toCommitment().toHex()).not.toBe(b.summary.toCommitment().toHex());
    } finally {
      a.anchor.free();
      b.anchor.free();
    }
  });

  it('refuses an approval expiration the auth procedure would clamp', async () => {
    await expect(buildRequest({ approvalExpirationDelta: 65_536 })).rejects.toThrow(
      /between 1 and 65535/,
    );
  });
});
