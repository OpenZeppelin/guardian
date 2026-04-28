#!/usr/bin/env node
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const skillDir = path.resolve(scriptDir, '..');
const repoRoot = path.resolve(skillDir, '../../..');
const evmClientPackage = path.join(repoRoot, 'packages/guardian-evm-client');
const clientDist = path.join(evmClientPackage, 'dist/index.js');
const viemEntry = path.join(evmClientPackage, 'node_modules/viem/_esm/index.js');

const {
  GuardianEvmClient,
  normalizeEvmAddress,
} = await import(pathToFileURL(clientDist).href);
const {
  createPublicClient,
  encodeAbiParameters,
  encodeFunctionData,
  http,
  keccak256,
  parseAbi,
} = await import(pathToFileURL(viemEntry).href);

const moduleAbi = parseAbi([
  'function getSignerCount(address account) view returns (uint256)',
  'function threshold(address account) view returns (uint64)',
  'function submitProposal(address account, bytes32 mode, bytes executionCalldata, bytes[] signatures) returns (bytes32)',
  'function submitted(bytes32 proposalId) view returns (bool)',
  'function submittedSignatureCounts(bytes32 proposalId) view returns (uint256)',
]);

const guardianUrl = envValue('GUARDIAN_URL', 'http://127.0.0.1:3000');
const rpcUrl = envValue('EVM_RPC_URL', 'http://127.0.0.1:8545');
const chainId = envNumber('EVM_CHAIN_ID', 31337);
const accountAddress = normalizeEvmAddress(
  envValue('EVM_ACCOUNT_ADDRESS', '0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC')
);
const moduleAddress = normalizeEvmAddress(requiredEnv('EVM_MODULE_ADDRESS'));
const signerOne = normalizeEvmAddress(
  envValue('EVM_SIGNER_ONE', '0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266')
);
const signerTwo = normalizeEvmAddress(
  envValue('EVM_SIGNER_TWO', '0x70997970C51812dc3A010C7d01b50e0d17dc79C8')
);
const mode = envValue('EVM_MODE', `0x${'0'.repeat(64)}`);
const executionCalldata = envValue(
  'EVM_EXECUTION_CALLDATA',
  `0x${Buffer.from(`guardian-evm-smoke:${Date.now()}`).toString('hex')}`
);

const provider = jsonRpcProvider(rpcUrl);
const publicClient = createPublicClient({ transport: http(rpcUrl) });
const networkConfig = {
  kind: 'evm',
  chainId,
  accountAddress,
  multisigModuleAddress: moduleAddress,
  rpcEndpoint: rpcUrl,
};

const chainIdFromRpc = await provider.request({ method: 'eth_chainId' });
const rpcChainId = Number.parseInt(String(chainIdFromRpc), 16);
assert(rpcChainId === chainId, `RPC chain ID ${rpcChainId} does not match EVM_CHAIN_ID ${chainId}`);

const signerCount = await publicClient.readContract({
  address: moduleAddress,
  abi: moduleAbi,
  functionName: 'getSignerCount',
  args: [accountAddress],
});
const threshold = await publicClient.readContract({
  address: moduleAddress,
  abi: moduleAbi,
  functionName: 'threshold',
  args: [accountAddress],
});
assert(Number(signerCount) >= 2, `module signer count is ${signerCount}`);
assert(Number(threshold) <= Number(signerCount), `module threshold ${threshold} exceeds signer count ${signerCount}`);

const clientOne = new GuardianEvmClient({
  guardianUrl,
  provider,
  networkConfig,
  signerAddress: signerOne,
});
const clientTwo = new GuardianEvmClient({
  guardianUrl,
  provider,
  networkConfig,
  signerAddress: signerTwo,
});

const configure = await clientOne.configure([signerOne, signerTwo]);
assert(configure.success === true, `configure failed: ${configure.message}`);

const payload = {
  kind: 'evm',
  mode,
  executionCalldata,
  signatures: [],
};
const created = await clientOne.createProposal(payload, Date.now());
const proposals = await clientOne.listProposals();
const createdIsListed = proposals.some((proposal) =>
  proposal.deltaPayload.executionCalldata === executionCalldata ||
  proposal.newCommitment === created.commitment
);
assert(createdIsListed, 'created proposal was not returned by listProposals');

await clientOne.signProposal(created.commitment, payload);
await clientTwo.signProposal(created.commitment, payload);
const fetched = await clientOne.getProposal(created.commitment);
assert(
  fetched.deltaPayload.signatures.length >= 2,
  `expected at least 2 signatures, got ${fetched.deltaPayload.signatures.length}`
);

const submitData = encodeFunctionData({
  abi: moduleAbi,
  functionName: 'submitProposal',
  args: [
    accountAddress,
    mode,
    executionCalldata,
    fetched.deltaPayload.signatures.map((signature) => signature.signature.signature),
  ],
});
const txHash = await provider.request({
  method: 'eth_sendTransaction',
  params: [
    {
      from: signerOne,
      to: moduleAddress,
      data: submitData,
    },
  ],
});
assert(typeof txHash === 'string', 'eth_sendTransaction returned a non-string transaction hash');
const receipt = await publicClient.waitForTransactionReceipt({ hash: txHash });
assert(receipt.status === 'success', `submit transaction failed with status ${receipt.status}`);

const proposalId = keccak256(
  encodeAbiParameters(
    [
      { type: 'uint256' },
      { type: 'address' },
      { type: 'bytes32' },
      { type: 'bytes32' },
    ],
    [BigInt(chainId), accountAddress, mode, keccak256(executionCalldata)]
  )
);
const submitted = await publicClient.readContract({
  address: moduleAddress,
  abi: moduleAbi,
  functionName: 'submitted',
  args: [proposalId],
});
const submittedSignatureCount = await publicClient.readContract({
  address: moduleAddress,
  abi: moduleAbi,
  functionName: 'submittedSignatureCounts',
  args: [proposalId],
});
assert(submitted === true, 'submitted(proposalId) is false');
assert(
  Number(submittedSignatureCount) >= 2,
  `submitted signature count is ${submittedSignatureCount}`
);

writeSummary({
  guardianUrl,
  rpcUrl,
  chainId,
  accountId: clientOne.accountId,
  accountAddress,
  moduleAddress,
  signerOne,
  signerTwo,
  signerCount: signerCount.toString(),
  threshold: threshold.toString(),
  proposalCommitment: created.commitment,
  proposalId,
  signatureCount: fetched.deltaPayload.signatures.length,
  signerIds: fetched.deltaPayload.signatures.map((signature) => signature.signerId),
  submitTxHash: txHash,
  submitted,
  submittedSignatureCount: submittedSignatureCount.toString(),
});

function jsonRpcProvider(url) {
  let id = 0;
  return {
    async request({ method, params = [] }) {
      id += 1;
      const response = await fetch(url, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id, method, params }),
      });
      const text = await response.text();
      let body;
      try {
        body = JSON.parse(text);
      } catch {
        throw new Error(`RPC ${method} returned non-JSON response: ${text}`);
      }
      if (!response.ok) {
        throw new Error(`RPC ${method} HTTP ${response.status}: ${text}`);
      }
      if (body.error) {
        throw new Error(`RPC ${method} error ${body.error.code ?? ''}: ${body.error.message}`);
      }
      return body.result;
    },
  };
}

function envValue(name, fallback) {
  const value = process.env[name];
  return value && value.length > 0 ? value : fallback;
}

function requiredEnv(name) {
  const value = process.env[name];
  if (!value || value.length === 0) {
    throw new Error(`${name} is required`);
  }
  return value;
}

function envNumber(name, fallback) {
  const value = process.env[name];
  if (!value || value.length === 0) {
    return fallback;
  }
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive safe integer`);
  }
  return parsed;
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function writeSummary(summary) {
  console.log(JSON.stringify(summary, null, 2));
}
