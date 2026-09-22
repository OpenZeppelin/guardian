import { MockWebClient, Word } from '@miden-sdk/miden-sdk';
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
