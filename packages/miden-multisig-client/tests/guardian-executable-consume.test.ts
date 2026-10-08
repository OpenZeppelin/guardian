import { Account, FaucetType, MidenClient, TransactionRequest, Word } from '@miden-sdk/miden-sdk';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { createMultisigAccount } from '../src/account/builder.js';
import { freshDeviceStore } from '../src/testing/fake-indexeddb-device.js';
import {
  buildConsumeNotesTransactionRequestFromNotes,
  buildPinnedConsumeNotesTransactionRequest,
} from '../src/transaction/consumeNotes.js';
import {
  GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA,
  GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA,
} from '../src/transaction/expiration.js';
import {
  executeForSummary,
  executeForSummaryAtTip,
  summaryApprovalExpirationBlockNum,
} from '../src/transaction/summary.js';

/**
 * A Guardian-executable consume-notes proposal pins every note with its inclusion proof, so a
 * party whose store has never seen the notes, which is Guardian's position, reproduces the
 * signed summary from the stored request bytes alone, at its own later tip.
 */
const SIGNER_COMMITMENT = '0x260a375ca01f1f05cd7bf22298b40c47290fc09f209011d39049b7f2ef61387b';
const GUARDIAN_COMMITMENT = '0xc35d79423c41d46b5289aafef48be2364e9ea494c6b14d6aefad10f1a46e6d7c';
const SALT_HEX = '0x' + '44'.repeat(32);

interface Proposed {
  accountId: string;
  accountBytes: Uint8Array;
  pinnedRequest: Uint8Array;
  unpinnedRequest: Uint8Array;
  summaryCommitment: string;
  selfExecutedCommitment: string;
  summaryExpirationDelta: number;
  approvalExpirationBlockNum: number | undefined;
  boundBlockNum: number;
  chain: Uint8Array;
}

async function propose(): Promise<Proposed> {
  const proposer = await MidenClient.createMock();
  try {
    const { account } = await createMultisigAccount(proposer, {
      threshold: 1,
      signerCommitments: [SIGNER_COMMITMENT],
      guardianCommitment: GUARDIAN_COMMITMENT,
      seed: new Uint8Array(32).fill(12),
    });
    const accountId = account.id().toString();
    const faucet = await proposer.accounts.create({
      type: FaucetType.FungibleFaucet,
      symbol: 'TOK',
      decimals: 8,
      maxSupply: 1_000_000n,
      storage: 'public',
    });
    await proposer.transactions.mint({ account: faucet, to: account, amount: 1_000n, type: 'public' });
    await proposer.proveBlock();
    await proposer.sync();

    const records = await proposer.notes.list({ status: 'committed' });
    expect(records.length).toBe(1);
    expect(records[0].inclusionProof()).toBeDefined();
    const notes = records.map((record) => record.toNote());
    const options = {
      accountId,
      salt: Word.fromHex(SALT_HEX),
      approvalExpirationDelta: GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA,
      transactionExpirationDelta: GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA,
    };
    const pinned = await buildPinnedConsumeNotesTransactionRequest(proposer, notes, options);
    const unpinned = await buildConsumeNotesTransactionRequestFromNotes(proposer, notes, options);
    const { summary, anchor } = await executeForSummary(proposer, accountId, pinned.request);
    const boundBlockNum = anchor.blockNum();
    anchor.free();
    const selfExecuted = await buildConsumeNotesTransactionRequestFromNotes(proposer, notes, {
      accountId,
      salt: Word.fromHex(SALT_HEX),
    });
    const selfExecutedSummary = await executeForSummary(proposer, accountId, selfExecuted.request);
    selfExecutedSummary.anchor.free();

    await proposer.proveBlock();
    await proposer.proveBlock();
    await proposer.proveBlock();
    return {
      accountId,
      accountBytes: account.serialize(),
      pinnedRequest: pinned.request.serialize(),
      unpinnedRequest: unpinned.request.serialize(),
      summaryCommitment: summary.toCommitment().toHex(),
      selfExecutedCommitment: selfExecutedSummary.summary.toCommitment().toHex(),
      summaryExpirationDelta: summary.expirationDelta(),
      approvalExpirationBlockNum: summaryApprovalExpirationBlockNum(summary),
      boundBlockNum,
      chain: await proposer.serializeMockChain(),
    };
  } finally {
    proposer.terminate();
  }
}

let proposed: Proposed;
let executor: MidenClient;

beforeAll(async () => {
  proposed = await propose();
  freshDeviceStore();
  executor = await MidenClient.createMock({ serializedMockChain: proposed.chain });
  await executor.accounts.insert({ account: Account.deserialize(proposed.accountBytes) });
  await executor.sync();
});

afterAll(() => {
  executor?.terminate();
});

function executeStored(bytes: Uint8Array) {
  return executeForSummaryAtTip(executor, proposed.accountId, TransactionRequest.deserialize(bytes));
}

describe('Guardian-executable consume-notes reproduction', () => {
  it('signs both expiration bounds', () => {
    expect(proposed.summaryExpirationDelta).toBe(GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA);
    expect(proposed.approvalExpirationBlockNum).toBe(
      proposed.boundBlockNum + GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA,
    );
  });

  it('makes the same effects a different proposal than the self-executed mode', () => {
    expect(proposed.selfExecutedCommitment).not.toBe(proposed.summaryCommitment);
  });

  it('reproduces a pinned request on a store that never saw the notes, at a later tip', async () => {
    expect(await executor.notes.list()).toEqual([]);
    expect(await executor.getSyncHeight()).toBeGreaterThan(proposed.boundBlockNum);

    const reproduced = await executeStored(proposed.pinnedRequest);

    expect(reproduced.toCommitment().toHex()).toBe(proposed.summaryCommitment);
  });

  it('does not reproduce the signed summary from a request that leaves the notes unpinned', async () => {
    const unpinned = await executeStored(proposed.unpinnedRequest);

    expect(unpinned.toCommitment().toHex()).not.toBe(proposed.summaryCommitment);
  });
});
