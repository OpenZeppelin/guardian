import {
  AdviceMap,
  FaucetType,
  FeltArray,
  MidenClient,
  Signature,
  TransactionRequest,
  TransactionSummary,
  Word,
} from '@miden-sdk/miden-sdk';
import type { DeltaProposalRequest, GuardianHttpClient, Signer } from '@openzeppelin/guardian-client';
import { secp256k1 } from '@noble/curves/secp256k1';
import { keccak_256 } from '@noble/hashes/sha3.js';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { createMultisigAccount } from '../src/account/builder.js';
import { Multisig } from '../src/multisig.js';
import type { ProposalExecutionMode } from '../src/transaction/expiration.js';
import { buildConsumeNotesTransactionRequestFromNotes } from '../src/transaction/consumeNotes.js';
import {
  GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA,
  GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA,
} from '../src/transaction/expiration.js';
import {
  chainAnchorFromBase64,
  executeForSummary,
  executeForSummaryAtTip,
  summaryApprovalExpirationBlockNum,
} from '../src/transaction/summary.js';
import { base64ToUint8Array, bytesToHex } from '../src/utils/encoding.js';
import {
  buildSignatureAdviceEntry,
  signatureHexToBytes,
  tryComputeEcdsaCommitmentHex,
} from '../src/utils/signature.js';
import { wordToBytes } from '../src/utils/word.js';

/**
 * A Guardian-executable P2ID proposal signs the 256-block transaction expiration and the default
 * approval window, and stores a request that reproduces its summary. Mirrors the Rust SDK's
 * `the_signer_set_and_payment_families_carry_both_bounds_and_reproduce` for the payment family.
 */
const SIGNER_KEY = new Uint8Array(32).fill(21);
const GUARDIAN_KEY = new Uint8Array(32).fill(22);
const RECIPIENT_ID = '0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b';
const SALT_HEX = '0x' + '55'.repeat(32);

function ecdsaCommitment(key: Uint8Array): { publicKey: string; commitment: string } {
  const publicKey = bytesToHex(secp256k1.getPublicKey(key, true));
  const commitment = tryComputeEcdsaCommitmentHex(publicKey);
  if (!commitment) {
    throw new Error('Could not derive an ECDSA commitment');
  }
  return { publicKey, commitment };
}

const signer = ecdsaCommitment(SIGNER_KEY);
const guardian = ecdsaCommitment(GUARDIAN_KEY);

function signatureAdvice(summary: TransactionSummary): AdviceMap {
  const commitmentHex = summary.toCommitment().toHex();
  const digest = keccak_256(wordToBytes(Word.fromHex(commitmentHex)));
  const advice = new AdviceMap();
  for (const [key, commitment] of [
    [SIGNER_KEY, signer.commitment],
    [GUARDIAN_KEY, guardian.commitment],
  ] as const) {
    const signature = secp256k1.sign(digest, key);
    const entry = buildSignatureAdviceEntry(
      Word.fromHex(commitment),
      Word.fromHex(commitmentHex),
      Signature.deserialize(
        signatureHexToBytes(
          bytesToHex(new Uint8Array([...signature.toCompactRawBytes(), signature.recovery])),
          'ecdsa',
        ),
      ),
    );
    advice.insert(entry.key, new FeltArray(entry.values));
  }
  return advice;
}

let client: MidenClient;
let accountId: string;
let faucetId: string;
let multisigAccount: Awaited<ReturnType<typeof createMultisigAccount>>['account'];

/** Mints to the multisig and consumes the note with real signatures, so it has funds to send. */
beforeAll(async () => {
  client = await MidenClient.createMock();
  const { account } = await createMultisigAccount(client, {
    threshold: 1,
    signerCommitments: [signer.commitment],
    guardianCommitment: guardian.commitment,
    signatureScheme: 'ecdsa',
    seed: new Uint8Array(32).fill(14),
  });
  multisigAccount = account;
  accountId = account.id().toString();
  const faucet = await client.accounts.create({
    type: FaucetType.FungibleFaucet,
    symbol: 'TOK',
    decimals: 8,
    maxSupply: 1_000_000n,
    storage: 'public',
  });
  faucetId = faucet.id().toString();
  await client.transactions.mint({ account: faucet, to: account, amount: 1_000n, type: 'public' });
  await client.proveBlock();
  await client.sync();

  const notes = (await client.notes.list({ status: 'committed' })).map((record) => record.toNote());
  const consumeOptions = { accountId, salt: Word.fromHex(SALT_HEX), signatureScheme: 'ecdsa' as const };
  const unsigned = await buildConsumeNotesTransactionRequestFromNotes(client, notes, consumeOptions);
  const { summary, anchor } = await executeForSummary(client, accountId, unsigned.request);
  const boundBlockNum = anchor.blockNum();
  anchor.free();
  const signed = await buildConsumeNotesTransactionRequestFromNotes(client, notes, {
    ...consumeOptions,
    boundBlockNum,
    signatureAdviceMap: signatureAdvice(summary),
  });
  await client.transactions.submit(accountId, signed.request);
  await client.proveBlock();
  await client.sync();
});

afterAll(() => {
  client?.terminate();
});

interface Pushed {
  summary: TransactionSummary;
  boundBlockNum: number;
  request: DeltaProposalRequest['deltaPayload']['transactionRequest'];
}

/** Creates a P2ID proposal under `mode` and returns what the client pushed to GUARDIAN. */
async function proposeP2id(mode: ProposalExecutionMode): Promise<Pushed> {
  const pushed: DeltaProposalRequest[] = [];
  const recordingGuardian = {
    pushDeltaProposal: async (request: DeltaProposalRequest) => {
      pushed.push(request);
      throw new Error('recorded');
    },
  } as unknown as GuardianHttpClient;
  const proposer = {
    scheme: 'ecdsa',
    commitment: signer.commitment,
    publicKey: signer.publicKey,
  } as unknown as Signer;
  const multisig = new Multisig(
    multisigAccount,
    {
      threshold: 1,
      signerCommitments: [signer.commitment],
      guardianCommitment: guardian.commitment,
    },
    recordingGuardian,
    proposer,
    client,
    accountId,
    'http://localhost:57291',
    undefined,
    undefined,
    mode,
  );
  await expect(multisig.createP2idProposal(RECIPIENT_ID, faucetId, 10n, { nonce: 2 })).rejects.toThrow(
    'recorded',
  );
  expect(pushed).toHaveLength(1);
  const payload = pushed[0].deltaPayload;
  const chainAnchor = payload.metadata?.chainAnchor;
  if (chainAnchor === undefined) {
    throw new Error('the pushed proposal carries no chain anchor');
  }
  const anchor = chainAnchorFromBase64(chainAnchor);
  const boundBlockNum = anchor.blockNum();
  anchor.free();
  return {
    summary: TransactionSummary.deserialize(base64ToUint8Array(payload.txSummary.data)),
    boundBlockNum,
    request: payload.transactionRequest,
  };
}

describe('Guardian-executable P2ID proposal', () => {
  it('signs the transaction expiration and the default approval window', async () => {
    const proposed = await proposeP2id('guardian_executable');

    expect(proposed.summary.expirationDelta()).toBe(GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA);
    expect(summaryApprovalExpirationBlockNum(proposed.summary)).toBe(
      proposed.boundBlockNum + GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA,
    );
  });

  it('stores a request that reproduces the signed summary', async () => {
    const proposed = await proposeP2id('guardian_executable');
    expect(proposed.request).toBeDefined();

    const stored = TransactionRequest.deserialize(base64ToUint8Array(proposed.request!.bytes));
    const reproduced = await executeForSummaryAtTip(client, accountId, stored);

    expect(reproduced.toCommitment().toHex()).toBe(proposed.summary.toCommitment().toHex());
  });

  it('signs neither bound on a self_executed client', async () => {
    const proposed = await proposeP2id('self_executed');

    expect(proposed.summary.expirationDelta()).toBe(0);
    expect(summaryApprovalExpirationBlockNum(proposed.summary)).toBeUndefined();
    expect(proposed.request).toBeUndefined();
  });
});
