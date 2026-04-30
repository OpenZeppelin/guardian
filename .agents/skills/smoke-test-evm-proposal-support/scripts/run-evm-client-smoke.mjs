#!/usr/bin/env node
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const skillDir = path.resolve(scriptDir, '..');
const repoRoot = path.resolve(skillDir, '../../..');
const evmClientPackage = path.join(repoRoot, 'packages/guardian-evm-client');
const clientDist = path.join(evmClientPackage, 'dist/index.js');
const viemEntry = path.join(evmClientPackage, 'node_modules/viem/_esm/index.js');
const viemAccountsEntry = path.join(evmClientPackage, 'node_modules/viem/_esm/accounts/index.js');

const {
  GuardianEvmClient,
  normalizeEvmAddress,
  signProposalHash,
} = await import(pathToFileURL(clientDist).href);
const {
  createPublicClient,
  http,
  keccak256,
  parseAbi,
  stringToHex,
} = await import(pathToFileURL(viemEntry).href);
const { privateKeyToAccount } = await import(pathToFileURL(viemAccountsEntry).href);

const validatorAbi = parseAbi([
  'function getSignerCount(address account) view returns (uint256)',
  'function threshold(address account) view returns (uint64)',
]);
const accountAbi = parseAbi([
  'function isModuleInstalled(uint256 moduleTypeId, address module, bytes additionalContext) view returns (bool)',
]);
const entrypointAbi = parseAbi([
  'function getNonce(address sender, uint192 key) view returns (uint256)',
]);
const defaultEntryPointAddress = '0x433709009b8330fda32311df1c2afa402ed8d009';

const guardianUrl = envValue('GUARDIAN_URL', 'http://127.0.0.1:3000');
const rpcUrl = envValue('EVM_RPC_URL', 'http://127.0.0.1:8545');
const chainId = envNumber('EVM_CHAIN_ID', 31337);
const smartAccountAddress = normalizeEvmAddress(requiredEnv('EVM_ACCOUNT_ADDRESS'));
const validatorAddress = normalizeEvmAddress(requiredEnv('EVM_VALIDATOR_ADDRESS'));
const entrypointAddress = normalizeEvmAddress(
  envValue('EVM_ENTRYPOINT_ADDRESS', defaultEntryPointAddress)
);
const signerOneKey = envValue(
  'EVM_SIGNER_ONE_PRIVATE_KEY',
  '0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80'
);
const signerTwoKey = envValue(
  'EVM_SIGNER_TWO_PRIVATE_KEY',
  '0x59c6995e998f97a5a0044966f094538d6f72b1d7c9d767f50fe9dc23c64fe4'
);
const signerOneAccount = privateKeyToAccount(signerOneKey);
const signerTwoAccount = privateKeyToAccount(signerTwoKey);
const signerOne = normalizeEvmAddress(signerOneAccount.address);
const signerTwo = normalizeEvmAddress(signerTwoAccount.address);
const userOpHash = envValue(
  'EVM_USER_OP_HASH',
  keccak256(stringToHex(`guardian-evm-smoke:${Date.now()}`))
);
const payload = envValue(
  'EVM_OPAQUE_PAYLOAD',
  JSON.stringify({ kind: 'userOperation', smoke: true, userOpHash })
);
const nonce = envValue('EVM_NONCE', '0');

const publicClient = createPublicClient({ transport: http(rpcUrl) });
const rpcProvider = jsonRpcProvider(rpcUrl);

const chainIdFromRpc = await rpcProvider.request({ method: 'eth_chainId' });
const rpcChainId = Number.parseInt(String(chainIdFromRpc), 16);
assert(rpcChainId === chainId, `RPC chain ID ${rpcChainId} does not match EVM_CHAIN_ID ${chainId}`);

const installed = await publicClient.readContract({
  address: smartAccountAddress,
  abi: accountAbi,
  functionName: 'isModuleInstalled',
  args: [1n, validatorAddress, '0x'],
});
assert(installed === true, 'validator is not installed on the smoke smart account');

const signerCount = await publicClient.readContract({
  address: validatorAddress,
  abi: validatorAbi,
  functionName: 'getSignerCount',
  args: [smartAccountAddress],
});
const threshold = await publicClient.readContract({
  address: validatorAddress,
  abi: validatorAbi,
  functionName: 'threshold',
  args: [smartAccountAddress],
});
const entrypointNonce = await publicClient.readContract({
  address: entrypointAddress,
  abi: entrypointAbi,
  functionName: 'getNonce',
  args: [smartAccountAddress, 0n],
});
assert(Number(signerCount) >= 2, `validator signer count is ${signerCount}`);
assert(Number(threshold) <= Number(signerCount), `threshold ${threshold} exceeds signer count ${signerCount}`);
assert(entrypointNonce === 0n, `entrypoint nonce is ${entrypointNonce}`);

const clientOne = new GuardianEvmClient({
  guardianUrl,
  provider: localWalletProvider(rpcProvider, signerOneAccount),
  signerAddress: signerOne,
});
const clientTwo = new GuardianEvmClient({
  guardianUrl,
  provider: localWalletProvider(rpcProvider, signerTwoAccount),
  signerAddress: signerTwo,
});

await clientOne.login();
await clientTwo.login();
await clientOne.configure({
  chainId,
  smartAccountAddress,
  multisigValidatorAddress: validatorAddress,
});
const accountId = clientOne.accountId(chainId, smartAccountAddress);

const initialSignature = await signProposalHash(clientOne.provider, signerOne, userOpHash);
const created = await clientOne.createProposal({
  accountId,
  chainId,
  smartAccountAddress,
  userOpHash,
  payload,
  nonce,
  signature: initialSignature,
  ttlSeconds: 900,
});

const listed = await clientOne.listProposals(accountId);
assert(
  listed.some((proposal) => proposal.proposalId === created.proposalId),
  'created proposal was not returned by listProposals'
);

const secondSignature = await signProposalHash(clientTwo.provider, signerTwo, userOpHash);
await clientTwo.approveProposal(accountId, created.proposalId, {
  signature: secondSignature,
});

const fetched = await clientOne.getProposal(accountId, created.proposalId);
assert(
  fetched.signatures.length >= 2,
  `expected at least 2 signatures, got ${fetched.signatures.length}`
);

const executable = await clientOne.getExecutableProposal(accountId, created.proposalId);
assert(executable.hash === userOpHash, 'executable hash mismatch');
assert(executable.payload === payload, 'executable payload mismatch');

writeSummary({
  guardianUrl,
  rpcUrl,
  chainId,
  smartAccountAddress,
  validatorAddress,
  entrypointAddress,
  signerOne,
  signerTwo,
  signerCount: signerCount.toString(),
  threshold: threshold.toString(),
  accountId,
  proposalId: created.proposalId,
  signatureCount: fetched.signatures.length,
  executableSigners: executable.signers,
});

function localWalletProvider(rpcProvider, account) {
  return {
    async request({ method, params = [] }) {
      if (method === 'eth_requestAccounts') {
        return [account.address];
      }
      if (method === 'eth_signTypedData_v4') {
        const typedData = JSON.parse(String(params[1]));
        return account.signTypedData({
          domain: typedData.domain,
          types: { GuardianEvmSession: typedData.types.GuardianEvmSession },
          primaryType: typedData.primaryType,
          message: typedData.message,
        });
      }
      if (method === 'eth_sign') {
        return account.sign({ hash: String(params[1]) });
      }
      return rpcProvider.request({ method, params });
    },
  };
}

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
