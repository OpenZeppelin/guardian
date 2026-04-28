# @openzeppelin/guardian-evm-client

TypeScript EVM client for Guardian server proposal workflows.

This package is intentionally isolated from `@openzeppelin/guardian-client`.
It talks to Guardian over HTTP and signs EVM request/proposal payloads with an
injected EIP-1193 wallet.

Guardian servers must be built and run with the Rust `evm` feature enabled.
Default server builds expose the schema but reject EVM config/auth/proposal
requests with stable code `evm_support_disabled`.

## Installation

```bash
npm install @openzeppelin/guardian-evm-client
```

## Setup

Run Guardian with EVM support and an allowed chain ID:

```bash
GUARDIAN_EVM_ALLOWED_CHAIN_IDS=31337 \
cargo run -p guardian-server --features evm --bin server
```

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

The canonical Guardian account ID is
`evm:<chainId>:<normalizedAccountAddress>`. `accountAddress` is used for
identity and EIP-712 typed-data domains; `multisigModuleAddress` is the
ERC-7579-style module Guardian reads for signer and threshold checks.

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

Fetch existing pending proposals for the configured account:

```typescript
const proposals = await client.listProposals();

for (const proposal of proposals) {
  console.log(proposal.accountId, proposal.nonce, proposal.status);
}
```

Fetch a specific proposal by Guardian proposal ID:

```typescript
const proposal = await client.getProposal(created.commitment);

console.log(proposal.deltaPayload.mode);
console.log(proposal.deltaPayload.executionCalldata);
console.log(proposal.deltaPayload.signatures);
```

The package signs Guardian request auth and proposal payloads with
`eth_signTypedData_v4` and manages the EVM Guardian HTTP flow directly.

EVM v1 supports EOA signers, pending proposal coordination, and client-side
on-chain submission. ERC-1271, weighted multisig, execution tracking, and EVM
reconciliation are out of scope.
