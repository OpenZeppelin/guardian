import { MidenClient, TransactionRequest, Word } from '@miden-sdk/miden-sdk';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { createMultisigAccount } from '../src/account/builder.js';
import {
  buildUpdateSignersTransactionRequest,
  requestBoundBlockNum,
  requestSaltHex,
} from '../src/transaction.js';

/**
 * Carries the multisig auth args through a real `TransactionRequest` built against
 * the SDK's mock chain, which is as close to the VM as this package gets without a
 * node.
 *
 * The builder tests in `src/transaction/feeWiring.test.ts` record calls against a
 * fake client, so they prove what the builders ask for and nothing about what the
 * SDK attaches. A request the WASM handed back without auth args, or with a
 * preimage the auth procedure cannot pipe, passes there and aborts in
 * `resolve_auth_args`; this layer is where that would show.
 */
const SIGNER_COMMITMENT = '0x260a375ca01f1f05cd7bf22298b40c47290fc09f209011d39049b7f2ef61387b';
const GUARDIAN_COMMITMENT = '0xc35d79423c41d46b5289aafef48be2364e9ea494c6b14d6aefad10f1a46e6d7c';
const SALT_HEX = '0x' + '11'.repeat(32);

let client: MidenClient;
let accountId: string;

beforeAll(async () => {
  client = await MidenClient.createMock();
  const { account } = await createMultisigAccount(
    client,
    {
      threshold: 1,
      signerCommitments: [SIGNER_COMMITMENT],
      guardianCommitment: GUARDIAN_COMMITMENT,
      seed: new Uint8Array(32).fill(9),
    },
    'mock',
  );
  accountId = account.id().toString();
});

afterAll(async () => {
  await client?.terminate();
});

async function buildRequest(boundBlockNum?: number): Promise<TransactionRequest> {
  const { request } = await buildUpdateSignersTransactionRequest(
    client,
    1,
    [SIGNER_COMMITMENT],
    { accountId, salt: Word.fromHex(SALT_HEX), boundBlockNum, midenRpcEndpoint: 'mock' },
  );
  return request;
}

describe('multisig auth args survive a real TransactionRequest', () => {
  it('sets the auth arg and leaves no fee conversion salt for the client to commit', async () => {
    const request = await buildRequest();

    expect(request.authArg()).toBeDefined();
    expect(request.feeConversionSalt()).toBeUndefined();
  });

  it('binds the proposal salt and the sync height in the auth-args preimage', async () => {
    const request = await buildRequest();

    expect(requestSaltHex(request)).toBe(SALT_HEX);
    expect(requestBoundBlockNum(request)).toBe(await client.getSyncHeight());
  });

  it('pins the bound block a rebuild names', async () => {
    const request = await buildRequest(0);

    expect(requestBoundBlockNum(request)).toBe(0);
    expect(requestSaltHex(request)).toBe(SALT_HEX);
  });

  it('carries the auth args across serialization', async () => {
    // Proposals ship serialized to co-signers, who execute the request on the other
    // side. Auth args lost on the wire would abort in the auth procedure there.
    const request = await buildRequest();

    const roundTripped = TransactionRequest.deserialize(request.serialize());

    expect(roundTripped.authArg()?.toHex()).toBe(request.authArg()?.toHex());
    expect(requestSaltHex(roundTripped)).toBe(SALT_HEX);
    expect(requestBoundBlockNum(roundTripped)).toBe(requestBoundBlockNum(request));
  });
});
