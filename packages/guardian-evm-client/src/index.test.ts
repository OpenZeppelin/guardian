import { describe, expect, it } from 'vitest';
import {
  EvmInjectedWalletSigner,
  GuardianEvmClient,
  RequestAuthPayload,
  buildEvmProposalTypedData,
  buildEvmRequestTypedData,
  evmAccountId,
  normalizeEvmAddress,
  type Eip1193Provider,
  type EvmNetworkConfig,
  type EvmProposalPayload,
} from './index.js';

const config: EvmNetworkConfig = {
  kind: 'evm',
  chainId: 31337,
  accountAddress: '0xE7f1725E7734CE288F8367e1Bb143E90bb3F0512',
  multisigModuleAddress: '0xCf7Ed3AccA5a467e9e704C703E8D87F634fB0Fc9',
  rpcEndpoint: 'http://localhost:8545',
};

const payload: EvmProposalPayload = {
  kind: 'evm',
  mode: `0x${'0'.repeat(64)}`,
  executionCalldata: '0x',
  signatures: [],
};

describe('guardian-evm-client', () => {
  it('builds canonical EVM account IDs', () => {
    expect(normalizeEvmAddress(config.accountAddress)).toBe('0xe7f1725e7734ce288f8367e1bb143e90bb3f0512');
    expect(evmAccountId(config.chainId, config.accountAddress)).toBe(
      'evm:31337:0xe7f1725e7734ce288f8367e1bb143e90bb3f0512'
    );
  });

  it('builds Guardian request typed data', () => {
    const typedData = buildEvmRequestTypedData(
      config,
      evmAccountId(config.chainId, config.accountAddress),
      42,
      RequestAuthPayload.fromRequest({ account_id: 'example' })
    );

    expect(typedData.domain).toMatchObject({
      name: 'Guardian EVM Request',
      version: '1',
      chainId: 31337,
      verifyingContract: '0xe7f1725e7734ce288f8367e1bb143e90bb3f0512',
    });
    expect(typedData.primaryType).toBe('GuardianRequest');
    expect(typedData.message.timestamp).toBe(42);
  });

  it('builds Guardian proposal typed data', () => {
    const typedData = buildEvmProposalTypedData(config, payload);

    expect(typedData.domain.name).toBe('Guardian EVM Proposal');
    expect(typedData.primaryType).toBe('GuardianProposal');
    expect(typedData.message.mode).toBe(payload.mode);
    expect(typedData.message.execution_calldata_hash).toMatch(/^0x[0-9a-f]{64}$/);
  });

  it('uses injected wallets for EIP-712 signatures', async () => {
    let request: { method: string; params?: unknown[] } | undefined;
    const provider: Eip1193Provider = {
      async request(args) {
        request = args;
        return `0x${'11'.repeat(65)}`;
      },
    };

    const signer = new EvmInjectedWalletSigner(provider, config, config.accountAddress);
    const signature = await signer.signEvmProposal(payload);

    expect(signature).toEqual({ scheme: 'ecdsa', signature: `0x${'11'.repeat(65)}` });
    expect(request?.method).toBe('eth_signTypedData_v4');
    expect(request?.params?.[0]).toBe('0xe7f1725e7734ce288f8367e1bb143e90bb3f0512');
  });

  it('wraps Guardian EVM account context', () => {
    const provider: Eip1193Provider = {
      async request() {
        return `0x${'11'.repeat(65)}`;
      },
    };

    const client = new GuardianEvmClient({
      guardianUrl: 'http://localhost:3000',
      provider,
      networkConfig: config,
      signerAddress: config.accountAddress,
    });

    expect(client.accountId).toBe('evm:31337:0xe7f1725e7734ce288f8367e1bb143e90bb3f0512');
    expect(client.networkConfig.accountAddress).toBe('0xe7f1725e7734ce288f8367e1bb143e90bb3f0512');
    expect(client.signer.publicKey).toBe('0xe7f1725e7734ce288f8367e1bb143e90bb3f0512');
  });
});
