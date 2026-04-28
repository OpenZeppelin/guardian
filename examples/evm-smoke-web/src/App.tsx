import { useMemo, useState } from 'react';
import {
  GuardianEvmClient,
  evmAccountId,
  normalizeEvmAddress,
  type DeltaObject,
  type EvmNetworkConfig,
  type EvmProposalPayload,
  type Eip1193Provider,
} from '@openzeppelin/guardian-evm-client';

const defaultMode = `0x${'0'.repeat(64)}`;
const submitProposalSelector = 'c4f668c2';

type StepLog = {
  label: string;
  value: string;
};

export default function App() {
  const [guardianUrl, setGuardianUrl] = useState('http://localhost:3000');
  const [chainId, setChainId] = useState(31337);
  const [accountAddress, setAccountAddress] = useState('');
  const [moduleAddress, setModuleAddress] = useState('');
  const [rpcEndpoint, setRpcEndpoint] = useState('http://localhost:8545');
  const [mode, setMode] = useState(defaultMode);
  const [executionCalldata, setExecutionCalldata] = useState('0x');
  const [walletAddress, setWalletAddress] = useState('');
  const [proposalCommitment, setProposalCommitment] = useState('');
  const [proposalDelta, setProposalDelta] = useState<DeltaObject | null>(null);
  const [logs, setLogs] = useState<StepLog[]>([]);
  const [busy, setBusy] = useState(false);

  const networkConfig = useMemo<EvmNetworkConfig | null>(() => {
    try {
      if (!accountAddress || !moduleAddress || !rpcEndpoint) {
        return null;
      }
      return {
        kind: 'evm',
        chainId,
        accountAddress: normalizeEvmAddress(accountAddress),
        multisigModuleAddress: normalizeEvmAddress(moduleAddress),
        rpcEndpoint,
      };
    } catch {
      return null;
    }
  }, [accountAddress, chainId, moduleAddress, rpcEndpoint]);

  const accountId = networkConfig ? evmAccountId(networkConfig.chainId, networkConfig.accountAddress) : '';
  const proposalPayload: EvmProposalPayload = {
    kind: 'evm',
    mode,
    executionCalldata,
    signatures: [],
  };

  async function connectWallet() {
    setBusy(true);
    try {
      const provider = requireWallet();
      const accounts = await provider.request({ method: 'eth_requestAccounts' });
      if (!Array.isArray(accounts) || typeof accounts[0] !== 'string') {
        throw new Error('Wallet did not return an account');
      }
      setWalletAddress(normalizeEvmAddress(accounts[0]));
      appendLog('wallet', accounts[0]);
    } catch (error) {
      appendLog('error', formatError(error));
    } finally {
      setBusy(false);
    }
  }

  async function configureAccount() {
    const config = requireNetworkConfig();
    setBusy(true);
    try {
      await ensureWalletChain(config);
      const client = createClient(config);
      const response = await client.configure([walletAddress]);
      appendLog('configure', response.message);
    } catch (error) {
      appendLog('error', formatError(error));
    } finally {
      setBusy(false);
    }
  }

  async function createProposal() {
    const config = requireNetworkConfig();
    setBusy(true);
    try {
      await ensureWalletChain(config);
      const client = createClient(config);
      const response = await client.createProposal(proposalPayload, Date.now());
      setProposalCommitment(response.commitment);
      setProposalDelta(response.delta);
      appendLog('proposal', response.commitment);
    } catch (error) {
      appendLog('error', formatError(error));
    } finally {
      setBusy(false);
    }
  }

  async function signProposal() {
    const config = requireNetworkConfig();
    setBusy(true);
    try {
      await ensureWalletChain(config);
      const client = createClient(config);
      const delta = await client.signProposal(proposalCommitment, proposalPayload);
      setProposalDelta(delta);
      appendLog('signatures', summarizeSignatures(delta));
    } catch (error) {
      appendLog('error', formatError(error));
    } finally {
      setBusy(false);
    }
  }

  async function refreshProposal() {
    const config = requireNetworkConfig();
    setBusy(true);
    try {
      await ensureWalletChain(config);
      const client = createClient(config);
      const delta = await client.getProposal(proposalCommitment);
      setProposalDelta(delta);
      appendLog('signatures', summarizeSignatures(delta));
    } catch (error) {
      appendLog('error', formatError(error));
    } finally {
      setBusy(false);
    }
  }

  async function submitOnChain() {
    const config = requireNetworkConfig();
    setBusy(true);
    try {
      await ensureWalletChain(config);
      const payload = requireStoredEvmPayload(proposalDelta);
      const txHash = await requireWallet().request({
        method: 'eth_sendTransaction',
        params: [
          {
            from: walletAddress,
            to: config.multisigModuleAddress,
            data: encodeSubmitProposal(config.accountAddress, payload),
          },
        ],
      });
      if (typeof txHash !== 'string') {
        throw new Error('Wallet returned a non-string transaction hash');
      }
      appendLog('submitted', txHash);
    } catch (error) {
      appendLog('error', formatError(error));
    } finally {
      setBusy(false);
    }
  }

  function createClient(config: EvmNetworkConfig): GuardianEvmClient {
    if (!walletAddress) {
      throw new Error('Connect a wallet first');
    }
    return new GuardianEvmClient({
      guardianUrl,
      provider: requireWallet(),
      networkConfig: config,
      signerAddress: walletAddress,
    });
  }

  function requireNetworkConfig(): EvmNetworkConfig {
    if (!networkConfig) {
      throw new Error('Complete the EVM account configuration');
    }
    return networkConfig;
  }

  function requireWallet(): Eip1193Provider {
    if (!window.ethereum) {
      throw new Error('Injected wallet not found');
    }
    return window.ethereum;
  }

  function appendLog(label: string, value: string) {
    setLogs((current) => [{ label, value }, ...current].slice(0, 8));
  }

  return (
    <main className="app-shell">
      <section className="panel">
        <div className="toolbar">
          <div>
            <h1>EVM Proposal Smoke</h1>
            <p>{accountId || 'evm:<chain_id>:<account_address>'}</p>
          </div>
          <button disabled={busy} onClick={connectWallet}>
            {walletAddress ? shortAddress(walletAddress) : 'Connect'}
          </button>
        </div>

        <div className="grid">
          <label>
            Guardian URL
            <input value={guardianUrl} onChange={(event) => setGuardianUrl(event.target.value)} />
          </label>
          <label>
            Chain ID
            <input
              type="number"
              min={1}
              value={chainId}
              onChange={(event) => setChainId(Number(event.target.value))}
            />
          </label>
          <label>
            Account address
            <input value={accountAddress} onChange={(event) => setAccountAddress(event.target.value)} />
          </label>
          <label>
            Multisig module
            <input value={moduleAddress} onChange={(event) => setModuleAddress(event.target.value)} />
          </label>
          <label>
            RPC endpoint
            <input value={rpcEndpoint} onChange={(event) => setRpcEndpoint(event.target.value)} />
          </label>
          <label>
            Mode
            <input value={mode} onChange={(event) => setMode(event.target.value)} />
          </label>
          <label className="wide">
            Execution calldata
            <textarea value={executionCalldata} onChange={(event) => setExecutionCalldata(event.target.value)} />
          </label>
        </div>

        <div className="actions">
          <button disabled={busy || !networkConfig || !walletAddress} onClick={configureAccount}>
            Configure
          </button>
          <button disabled={busy || !networkConfig || !walletAddress} onClick={createProposal}>
            Create proposal
          </button>
          <button disabled={busy || !networkConfig || !walletAddress || !proposalCommitment} onClick={signProposal}>
            Sign proposal
          </button>
          <button disabled={busy || !networkConfig || !walletAddress || !proposalCommitment} onClick={refreshProposal}>
            Refresh proposal
          </button>
          <button disabled={busy || !networkConfig || !walletAddress || !proposalDelta} onClick={submitOnChain}>
            Submit on-chain
          </button>
        </div>
      </section>

      <section className="log-panel">
        {logs.map((entry, index) => (
          <div className="log-row" key={`${entry.label}-${index}`}>
            <strong>{entry.label}</strong>
            <span>{entry.value}</span>
          </div>
        ))}
      </section>
    </main>
  );
}

function summarizeSignatures(delta: DeltaObject): string {
  return `${delta.deltaPayload.signatures.length} stored`;
}

function requireStoredEvmPayload(delta: DeltaObject | null): EvmProposalPayload {
  if (!delta) {
    throw new Error('Refresh or create a proposal first');
  }
  return delta.deltaPayload;
}

function encodeSubmitProposal(accountAddress: string, payload: EvmProposalPayload): `0x${string}` {
  const account = encodeAddress(accountAddress);
  const mode = normalizeBytes32(payload.mode, 'mode');
  const executionCalldata = encodeBytes(payload.executionCalldata, 'execution calldata');
  const signatures = encodeBytesArray(
    payload.signatures.map((entry) => {
      if (entry.signature.scheme !== 'ecdsa') {
        throw new Error('EVM on-chain submit requires ECDSA signatures');
      }
      return entry.signature.signature;
    })
  );
  const executionOffset = encodeUint256(32 * 4);
  const signaturesOffset = encodeUint256(32 * 4 + hexByteLength(executionCalldata));
  return `0x${submitProposalSelector}${account}${mode}${executionOffset}${signaturesOffset}${executionCalldata}${signatures}`;
}

function encodeAddress(value: string): string {
  const address = strip0x(normalizeEvmAddress(value));
  return `${'0'.repeat(24)}${address}`;
}

function encodeBytesArray(values: string[]): string {
  const encodedValues = values.map((value) => encodeBytes(value, 'signature'));
  let offset = values.length * 32;
  const offsets = encodedValues.map((value) => {
    const current = offset;
    offset += hexByteLength(value);
    return encodeUint256(current);
  });
  return `${encodeUint256(values.length)}${offsets.join('')}${encodedValues.join('')}`;
}

function encodeBytes(value: string, field: string): string {
  const hex = strip0x(value);
  if (hex.length % 2 !== 0) {
    throw new Error(`${field} must contain an even number of hex characters`);
  }
  return `${encodeUint256(hex.length / 2)}${padRightWord(hex)}`;
}

function normalizeBytes32(value: string, field: string): string {
  const hex = strip0x(value).toLowerCase();
  if (hex.length !== 64) {
    throw new Error(`${field} must be 32 bytes`);
  }
  return hex;
}

function encodeUint256(value: number): string {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error('ABI integer value must be a non-negative safe integer');
  }
  return value.toString(16).padStart(64, '0');
}

function padRightWord(hex: string): string {
  const remainder = hex.length % 64;
  return remainder === 0 ? hex : `${hex}${'0'.repeat(64 - remainder)}`;
}

function hexByteLength(hex: string): number {
  return hex.length / 2;
}

function strip0x(value: string): string {
  if (!value.startsWith('0x')) {
    throw new Error('Expected 0x-prefixed hex');
  }
  return value.slice(2);
}

function shortAddress(address: string): string {
  return `${address.slice(0, 6)}...${address.slice(-4)}`;
}

async function ensureWalletChain(config: EvmNetworkConfig): Promise<void> {
  const provider = requireWindowEthereum();
  const chainId = toRpcChainId(config.chainId);
  try {
    await provider.request({
      method: 'wallet_switchEthereumChain',
      params: [{ chainId }],
    });
  } catch (error) {
    if (!isUnknownChainError(error)) {
      throw error;
    }
    await provider.request({
      method: 'wallet_addEthereumChain',
      params: [
        {
          chainId,
          chainName: `Guardian EVM ${config.chainId}`,
          nativeCurrency: { name: 'Ether', symbol: 'ETH', decimals: 18 },
          rpcUrls: [config.rpcEndpoint],
        },
      ],
    });
  }
}

function requireWindowEthereum(): Eip1193Provider {
  if (!window.ethereum) {
    throw new Error('Injected wallet not found');
  }
  return window.ethereum;
}

function toRpcChainId(chainId: number): `0x${string}` {
  if (!Number.isSafeInteger(chainId) || chainId <= 0) {
    throw new Error('Chain ID must be a positive safe integer');
  }
  return `0x${chainId.toString(16)}`;
}

function isUnknownChainError(error: unknown): boolean {
  if (!isRecord(error)) {
    return false;
  }
  return error.code === 4902 || error.code === -32603;
}

function formatError(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  if (isRecord(error)) {
    const code = typeof error.code === 'number' || typeof error.code === 'string' ? `code ${error.code}` : undefined;
    const message = typeof error.message === 'string' ? error.message : undefined;
    const data = formatErrorData(error.data);
    const details = [code, message, data].filter(Boolean).join(': ');
    return details || JSON.stringify(error);
  }
  return String(error);
}

function formatErrorData(data: unknown): string | undefined {
  if (typeof data === 'string') {
    return data;
  }
  if (!isRecord(data)) {
    return undefined;
  }
  return JSON.stringify(data);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}
