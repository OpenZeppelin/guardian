# @openzeppelin/guardian-evm-client

TypeScript EVM client for Guardian server proposal workflows.

## Installation

```bash
npm install @openzeppelin/guardian-evm-client
```

## Setup

```typescript
import { GuardianEvmClient, evmAccountId } from '@openzeppelin/guardian-evm-client';

const networkConfig = {
  kind: 'evm',
  chainId: 31337,
  accountAddress: '0x...',
  multisigModuleAddress: '0x...',
  rpcEndpoint: 'http://localhost:8545',
} as const;

const accounts = await window.ethereum.request({ method: 'eth_requestAccounts' });
const signerAddress = accounts[0] as string;

const client = new GuardianEvmClient({
  guardianUrl: 'http://localhost:3000',
  provider: window.ethereum,
  networkConfig,
  signerAddress,
});

console.log(evmAccountId(networkConfig.chainId, networkConfig.accountAddress));
```

## Usage

```typescript
await client.configure([signerAddress]);

const payload = {
  kind: 'evm',
  mode: `0x${'0'.repeat(64)}`,
  executionCalldata: '0x',
  signatures: [],
} as const;

const created = await client.createProposal(payload, Date.now());
const signed = await client.signProposal(created.commitment, payload);

console.log(signed.deltaPayload.signatures.length);
```

The package signs Guardian request auth and proposal payloads with
`eth_signTypedData_v4` and manages the EVM Guardian HTTP flow directly.
