import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  GuardianEvmClient,
  GuardianEvmHttpError,
  buildEvmSessionTypedData,
  evmAccountId,
  normalizeEvmAddress,
  signProposalHash,
  type Eip1193Provider,
} from './index.js';

const address = '0xE7f1725E7734CE288F8367e1Bb143E90bb3F0512';
const normalizedAddress = '0xe7f1725e7734ce288f8367e1bb143e90bb3f0512';
const validator = '0x1111111111111111111111111111111111111111';
const accountId = `evm:31337:${normalizedAddress}`;
const hash = `0x${'12'.repeat(32)}` as const;
const signature = `0x${'11'.repeat(65)}` as const;
const secondSigner = '0x2222222222222222222222222222222222222222';

describe('guardian-evm-client', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('normalizes EVM addresses and account ids', () => {
    expect(normalizeEvmAddress(address)).toBe(normalizedAddress);
    expect(evmAccountId(31337, address)).toBe(accountId);
  });

  it('builds Guardian EVM session typed data', () => {
    const typedData = buildEvmSessionTypedData({
      address,
      nonce: `0x${'ab'.repeat(32)}`,
      issuedAt: 1,
      expiresAt: 2,
    });

    expect(typedData.domain).toEqual({ name: 'Guardian EVM Session', version: '1' });
    expect(typedData.primaryType).toBe('GuardianEvmSession');
    expect(typedData.message.wallet).toBe(normalizedAddress);
  });

  it('logs in with an EIP-712 challenge and cookie fetch mode', async () => {
    const provider = providerReturning(signature);
    const fetchMock = mockFetch([
      {
        address: normalizedAddress,
        nonce: `0x${'aa'.repeat(32)}`,
        issued_at: 10,
        expires_at: 20,
      },
      {
        address: normalizedAddress,
        expires_at: 1234,
      },
    ]);
    const client = new GuardianEvmClient({
      guardianUrl: 'http://guardian.test',
      provider,
      signerAddress: address,
    });

    const session = await client.login();

    expect(session.address).toBe(normalizedAddress);
    expect(provider.request).toHaveBeenCalledWith(
      expect.objectContaining({ method: 'eth_signTypedData_v4' })
    );
    expect(fetchMock.mock.calls[0]?.[0]).toContain('/evm/auth/challenge');
    expect(fetchMock.mock.calls[1]?.[0]).toContain('/evm/auth/verify');
    expect(fetchMock.mock.calls[0]?.[1]).toMatchObject({ credentials: 'include' });
  });

  it('registers an EVM account through /evm/accounts', async () => {
    const fetchMock = mockFetch([
      {
        account_id: accountId,
        chain_id: 31337,
        account_address: normalizedAddress,
        multisig_validator_address: validator,
        signers: [normalizedAddress, secondSigner],
        threshold: 2,
      },
    ]);
    const client = new GuardianEvmClient({ guardianUrl: 'http://guardian.test' });

    const response = await client.configure({
      chainId: 31337,
      smartAccountAddress: address,
      multisigValidatorAddress: validator,
    });

    expect(response.accountId).toBe(accountId);
    expect(fetchMock.mock.calls[0]?.[0]).toBe('http://guardian.test/evm/accounts');
    const body = JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body));
    expect(body).toEqual({
      chain_id: 31337,
      account_address: normalizedAddress,
      multisig_validator_address: validator,
    });
  });

  it('creates proposals through /evm/proposals', async () => {
    const fetchMock = mockFetch([proposal({ signatureCount: 1 })]);
    const client = new GuardianEvmClient({ guardianUrl: 'http://guardian.test' });

    const response = await client.createProposal({
      chainId: 31337,
      smartAccountAddress: address,
      userOpHash: hash,
      payload: '{"kind":"userOp"}',
      nonce: '0x01',
      signature,
      ttlSeconds: 300,
    });

    expect(response.proposalId).toBe(hash);
    expect(response.signatures).toHaveLength(1);
    expect(fetchMock.mock.calls[0]?.[0]).toBe('http://guardian.test/evm/proposals');
    const body = JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body));
    expect(body).toEqual({
      account_id: accountId,
      user_op_hash: hash,
      payload: '{"kind":"userOp"}',
      nonce: '0x01',
      ttl_seconds: 300,
      signature,
    });
  });

  it('fetches and converts proposals from /evm/proposals', async () => {
    mockFetch([{ proposals: [proposal({ signatureCount: 1 })] }]);
    const client = new GuardianEvmClient({ guardianUrl: 'http://guardian.test' });

    const proposals = await client.listProposals(accountId);

    expect(proposals[0]?.proposalId).toBe(hash);
    expect(proposals[0]?.signatures[0]?.signedAt).toBe(42);
  });

  it('approves proposals with raw ECDSA signatures', async () => {
    const fetchMock = mockFetch([proposal({ signatureCount: 2 })]);
    const client = new GuardianEvmClient({ guardianUrl: 'http://guardian.test' });

    const updated = await client.approveProposal(accountId, hash, { signature });

    expect(updated.signatures).toHaveLength(2);
    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      `http://guardian.test/evm/proposals/${hash}/approve`
    );
    const body = JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body));
    expect(body).toEqual({ account_id: accountId, signature });
  });

  it('fetches executable proposal data and cancels proposals', async () => {
    const fetchMock = mockFetch([
      {
        hash,
        payload: '{}',
        signatures: [signature],
        signers: [normalizedAddress],
      },
      { success: true },
    ]);
    const client = new GuardianEvmClient({ guardianUrl: 'http://guardian.test' });

    const executable = await client.getExecutableProposal(accountId, hash);
    await client.cancelProposal(accountId, hash);

    expect(executable.hash).toBe(hash);
    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      `http://guardian.test/evm/proposals/${hash}/executable?account_id=${encodeURIComponent(accountId)}`
    );
    expect(fetchMock.mock.calls[1]?.[0]).toBe(
      `http://guardian.test/evm/proposals/${hash}/cancel`
    );
  });

  it('parses stable error codes', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify({ code: 'unsupported_for_network' }), { status: 400 }))
    );
    const client = new GuardianEvmClient({ guardianUrl: 'http://guardian.test' });

    await expect(client.challenge(address)).rejects.toMatchObject<Partial<GuardianEvmHttpError>>({
      code: 'unsupported_for_network',
    });
  });

  it('can request hash signatures from injected wallets', async () => {
    const provider = providerReturning(signature);

    const result = await signProposalHash(provider, address, hash);

    expect(result).toBe(signature);
    expect(provider.request).toHaveBeenCalledWith({
      method: 'eth_sign',
      params: [normalizedAddress, hash],
    });
  });
});

function providerReturning(result: string): Eip1193Provider {
  return {
    request: vi.fn(async () => result),
  };
}

function mockFetch(jsonResponses: unknown[]) {
  const fetchMock = vi.fn(async () => {
    const response = jsonResponses.shift();
    return new Response(JSON.stringify(response), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    });
  });
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}

function proposal({ signatureCount }: { signatureCount: number }) {
  const signatures = Array.from({ length: signatureCount }, (_, index) => ({
    signer: index === 0 ? normalizedAddress : secondSigner,
    signature,
    signed_at: 42 + index,
  }));
  return {
    proposal_id: hash,
    account_id: accountId,
    chain_id: 31337,
    smart_account_address: normalizedAddress,
    validator_address: validator,
    user_op_hash: hash,
    payload: '{}',
    nonce: '1',
    nonce_key: '0',
    proposer: normalizedAddress,
    signer_snapshot: [normalizedAddress, secondSigner],
    threshold: 2,
    signatures,
    created_at: 1,
    expires_at: 2,
  };
}
