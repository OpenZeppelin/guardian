# @openzeppelin/guardian-evm-client

TypeScript EVM client for Guardian Accounts-compatible proposal coordination.

This package is intentionally isolated from `@openzeppelin/guardian-client`.
It talks to Guardian's EVM-enabled HTTP routes, uses a wallet-derived cookie
session, registers EVM accounts through `/evm/accounts`, and treats proposal
payloads as opaque application data.

Guardian servers must be built and run with the Rust `evm` feature enabled.
Default server builds do not register the `/evm/*` routes.

## Setup

Run Guardian with server-owned RPC URLs and the shared EntryPoint v0.9 address:

```bash
GUARDIAN_EVM_RPC_URLS=31337=http://127.0.0.1:8545 \
GUARDIAN_EVM_ENTRYPOINT_ADDRESS=0x433709009b8330fda32311df1c2afa402ed8d009 \
cargo run -p guardian-server --features evm --bin server
```

```typescript
import { GuardianEvmClient, signProposalHash } from '@openzeppelin/guardian-evm-client';

const accounts = await window.ethereum.request({ method: 'eth_requestAccounts' });
const signerAddress = accounts[0] as `0x${string}`;

const client = new GuardianEvmClient({
  guardianUrl: 'http://localhost:3000',
  provider: window.ethereum,
  signerAddress,
});

await client.login();
```

## Configure

Configure the smart account once before creating proposals. Guardian resolves
RPC URLs from the server chain map and uses one server-owned EntryPoint address;
clients only provide the chain, smart account, and validator addresses.

```typescript
await client.configure({
  chainId: 31337,
  smartAccountAddress: '0x...',
  multisigValidatorAddress: '0x...',
});

const accountId = client.accountId(31337, '0x...');
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

The integrating app builds the UserOperation, computes the hash to be signed,
collects wallet signatures, and sends the opaque payload to Guardian.

```typescript
const hash = '0x...' as const;
const signature = await signProposalHash(window.ethereum, signerAddress, hash);

const created = await client.createProposal({
  chainId: 31337,
  smartAccountAddress: '0x...',
  userOpHash: hash,
  payload: JSON.stringify({ packedUserOperation }),
  nonce: '0',
  signature,
  ttlSeconds: 900,
});

const proposals = await client.listProposals(accountId);
const proposal = await client.getProposal(accountId, created.proposalId);
```

Approve with another signer session:

```typescript
const approval = await signProposalHash(window.ethereum, otherSigner, proposal.userOpHash);

await client.approveProposal(accountId, proposal.proposalId, {
  signature: approval,
});
```

When the signature threshold is met, fetch the payload and ordered signatures
for the app's own submission flow:

```typescript
const executable = await client.getExecutableProposal(accountId, proposal.proposalId);

console.log(executable.payload, executable.signatures, executable.signers);
```

Cancel a pending proposal as its creator:

```typescript
await client.cancelProposal(accountId, proposal.proposalId);
```

Guardian verifies the validator is installed, snapshots EOA signers from the
validator, verifies signatures over the supplied hash, and lazily removes
expired or finalized proposals. Guardian does not build UserOperations, decode
payloads, or submit transactions on-chain.
