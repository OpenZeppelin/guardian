import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';
import { AuthSecretKey, MidenClient } from '@miden-sdk/miden-sdk';

import { MultisigClient } from '../../src/client.js';
import { EcdsaSigner } from '../../src/signers/ecdsa.js';
import { FalconSigner } from '../../src/signers/falcon.js';
import { hexToBytes } from '../../src/utils/encoding.js';

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} must be set to cosign`);
  return value;
}

/**
 * Signs one proposal on an account this process has never seen, using only a
 * handed-over key. The Rust driver invokes this for the cross-SDK handoff, so
 * the signature genuinely comes from the TypeScript SDK.
 */
describe('cross-SDK cosigner', () => {
  it('signs the handed-over proposal', async () => {
    const accountId = required('QUAL_COSIGN_ACCOUNT_ID');
    const proposalId = required('QUAL_COSIGN_PROPOSAL_ID');
    const scheme = required('QUAL_COSIGN_SCHEME') as 'falcon' | 'ecdsa';
    const keyHex = readFileSync(required('QUAL_COSIGN_KEY_FILE'), 'utf8').trim();

    const secretKey = AuthSecretKey.deserialize(hexToBytes(keyHex));
    const signer = scheme === 'falcon' ? new FalconSigner(secretKey) : new EcdsaSigner(secretKey);

    const midenClient = await MidenClient.create({
      rpcUrl: required('QUAL_MIDEN_RPC_ENDPOINT'),
      proverUrl: process.env.QUAL_TS_PROVER ?? required('QUAL_NETWORK'),
      storeName: `qual-cosign-${Date.now()}`,
      autoSync: false,
    });

    const multisigClient = new MultisigClient(midenClient, {
      guardianEndpoint: required('QUAL_HTTP_ENDPOINT'),
      midenRpcEndpoint: required('QUAL_MIDEN_RPC_ENDPOINT'),
    });

    const multisig = await multisigClient.load(accountId, signer);
    await midenClient.sync();
    await multisig.syncState();
    await multisig.syncProposals();
    const signed = await multisig.signProposal(proposalId);

    expect(signed.signatures.map((entry) => entry.signerId.toLowerCase())).toContain(
      signer.commitment.toLowerCase(),
    );
  });
});
