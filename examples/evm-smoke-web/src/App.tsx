import { useMemo, useState } from 'react';
import {
  GuardianEvmClient,
  normalizeBytes32,
  normalizeEvmAddress,
  signProposalHash,
  type Eip1193Provider,
  type Proposal,
} from '@openzeppelin/guardian-evm-client';

type StepLog = {
  label: string;
  value: string;
};

const defaultPayload = JSON.stringify({ kind: 'userOperation', callData: '0x' }, null, 2);
const defaultHash = `0x${'12'.repeat(32)}`;

export default function App() {
  const [guardianUrl, setGuardianUrl] = useState('');
  const [chainId, setChainId] = useState(31337);
  const [accountAddress, setAccountAddress] = useState('');
  const [validatorAddress, setValidatorAddress] = useState('');
  const [userOpHash, setUserOpHash] = useState(defaultHash);
  const [payload, setPayload] = useState(defaultPayload);
  const [nonce, setNonce] = useState('0');
  const [ttlSeconds, setTtlSeconds] = useState(900);
  const [walletAddress, setWalletAddress] = useState('');
  const [proposalId, setProposalId] = useState('');
  const [proposal, setProposal] = useState<Proposal | null>(null);
  const [logs, setLogs] = useState<StepLog[]>([]);
  const [busy, setBusy] = useState(false);

  const client = useMemo(
    () =>
      new GuardianEvmClient({
        guardianUrl,
        provider: window.ethereum,
        signerAddress: walletAddress || undefined,
      }),
    [guardianUrl, walletAddress]
  );

  async function connectWallet() {
    setBusy(true);
    try {
      const provider = requireWallet();
      const accounts = await provider.request({ method: 'eth_requestAccounts' });
      if (!Array.isArray(accounts) || typeof accounts[0] !== 'string') {
        throw new Error('Wallet did not return an account');
      }
      const address = normalizeEvmAddress(accounts[0]);
      setWalletAddress(address);
      appendLog('wallet', address);
    } catch (error) {
      appendLog('error', formatError(error));
    } finally {
      setBusy(false);
    }
  }

  async function login() {
    setBusy(true);
    try {
      const address = requireWalletAddress();
      await ensureWalletChain(chainId);
      const session = await client.login(address);
      appendLog('session', `${session.address} until ${new Date(session.expiresAt).toLocaleTimeString()}`);
    } catch (error) {
      appendLog('error', formatError(error));
    } finally {
      setBusy(false);
    }
  }

  async function createProposal() {
    setBusy(true);
    try {
      const signer = requireWalletAddress();
      const normalizedHash = normalizeBytes32(userOpHash, 'hash');
      await ensureWalletChain(chainId);
      const account = await client.configure({
        chainId,
        smartAccountAddress: accountAddress,
        multisigValidatorAddress: validatorAddress,
      });
      appendLog('configured', `${account.accountId} (${account.threshold} threshold)`);
      const signature = await signProposalHash(requireWallet(), signer, normalizedHash);
      const response = await client.createProposal({
        chainId,
        smartAccountAddress: accountAddress,
        userOpHash: normalizedHash,
        payload,
        nonce,
        signature,
        ttlSeconds,
      });
      setProposalId(response.proposalId);
      appendLog('proposal', `${response.proposalId} (${response.signatures.length} signature)`);
      await refreshProposal(response.proposalId);
    } catch (error) {
      appendLog('error', formatError(error));
    } finally {
      setBusy(false);
    }
  }

  async function approveProposal() {
    setBusy(true);
    try {
      const signer = requireWalletAddress();
      const id = requireProposalId();
      await ensureWalletChain(chainId);
      const signature = await signProposalHash(requireWallet(), signer, normalizeBytes32(userOpHash, 'hash'));
      const proposal = await client.approveProposal(accountId(), id, {
        signature,
      });
      appendLog('approved', `${proposal.signatures.length} signature(s)`);
      await refreshProposal(id);
    } catch (error) {
      appendLog('error', formatError(error));
    } finally {
      setBusy(false);
    }
  }

  async function listProposals() {
    setBusy(true);
    try {
      const proposals = await client.listProposals(accountId());
      appendLog('list', `${proposals.length} proposal(s)`);
      if (proposals[0]) {
        setProposalId(proposals[0].proposalId);
        setProposal(proposals[0]);
      }
    } catch (error) {
      appendLog('error', formatError(error));
    } finally {
      setBusy(false);
    }
  }

  async function refreshProposal(id = proposalId) {
    setBusy(true);
    try {
      const proposal = await client.getProposal(accountId(), id || requireProposalId());
      setProposalId(proposal.proposalId);
      setProposal(proposal);
      appendLog('signatures', `${proposal.signatures.length} stored`);
    } catch (error) {
      appendLog('error', formatError(error));
    } finally {
      setBusy(false);
    }
  }

  async function getExecutable() {
    setBusy(true);
    try {
      const executable = await client.getExecutableProposal(accountId(), requireProposalId());
      appendLog('executable', `${executable.signatures.length} signature(s), payload ${executable.payload.length} chars`);
    } catch (error) {
      appendLog('error', formatError(error));
    } finally {
      setBusy(false);
    }
  }

  async function cancelProposal() {
    setBusy(true);
    try {
      await client.cancelProposal(accountId(), requireProposalId());
      setProposal(null);
      setProposalId('');
      appendLog('cancel', 'proposal removed');
    } catch (error) {
      appendLog('error', formatError(error));
    } finally {
      setBusy(false);
    }
  }

  function requireWalletAddress(): `0x${string}` {
    if (!walletAddress) {
      throw new Error('Connect a wallet first');
    }
    return normalizeEvmAddress(walletAddress);
  }

  function requireProposalId(): string {
    if (!proposalId) {
      throw new Error('Create or select a proposal first');
    }
    return proposalId;
  }

  function accountId(): string {
    return client.accountId(chainId, accountAddress);
  }

  function requireWallet(): Eip1193Provider {
    if (!window.ethereum) {
      throw new Error('Injected wallet not found');
    }
    return window.ethereum;
  }

  function appendLog(label: string, value: string) {
    setLogs((current) => [{ label, value }, ...current].slice(0, 10));
  }

  return (
    <main className="app-shell">
      <section className="panel">
        <div className="toolbar">
          <div>
            <h1>EVM Proposal Smoke</h1>
            <p>{proposalId || 'No proposal selected'}</p>
          </div>
          <button disabled={busy} onClick={connectWallet}>
            {walletAddress ? shortAddress(walletAddress) : 'Connect'}
          </button>
        </div>

        <div className="grid">
          <label>
            Guardian URL
            <input placeholder="blank uses Vite proxy" value={guardianUrl} onChange={(event) => setGuardianUrl(event.target.value)} />
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
            Smart account
            <input value={accountAddress} onChange={(event) => setAccountAddress(event.target.value)} />
          </label>
          <label>
            Multisig validator
            <input value={validatorAddress} onChange={(event) => setValidatorAddress(event.target.value)} />
          </label>
          <label>
            UserOp hash
            <input value={userOpHash} onChange={(event) => setUserOpHash(event.target.value)} />
          </label>
          <label>
            Nonce
            <input value={nonce} onChange={(event) => setNonce(event.target.value)} />
          </label>
          <label>
            TTL seconds
            <input
              type="number"
              min={1}
              value={ttlSeconds}
              onChange={(event) => setTtlSeconds(Number(event.target.value))}
            />
          </label>
          <label className="wide">
            Opaque payload
            <textarea value={payload} onChange={(event) => setPayload(event.target.value)} />
          </label>
        </div>

        <div className="actions">
          <button disabled={busy || !walletAddress} onClick={login}>
            Login
          </button>
          <button disabled={busy || !walletAddress || !accountAddress || !validatorAddress} onClick={createProposal}>
            Create
          </button>
          <button disabled={busy || !walletAddress || !proposalId} onClick={approveProposal}>
            Approve
          </button>
          <button disabled={busy} onClick={listProposals}>
            List
          </button>
          <button disabled={busy || !proposalId} onClick={() => refreshProposal()}>
            Refresh
          </button>
          <button disabled={busy || !proposalId} onClick={getExecutable}>
            Executable
          </button>
          <button disabled={busy || !proposalId} onClick={cancelProposal}>
            Cancel
          </button>
        </div>
      </section>

      {proposal ? (
        <section className="log-panel">
          <div className="log-row">
            <strong>proposal</strong>
            <span>{proposal.proposalId}</span>
          </div>
          <div className="log-row">
            <strong>hash</strong>
            <span>{proposal.userOpHash}</span>
          </div>
          <div className="log-row">
            <strong>signatures</strong>
            <span>{proposal.signatures.map((entry) => shortAddress(entry.signer)).join(', ') || 'none'}</span>
          </div>
        </section>
      ) : null}

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

async function ensureWalletChain(chainId: number): Promise<void> {
  const provider = requireWindowEthereum();
  try {
    await provider.request({
      method: 'wallet_switchEthereumChain',
      params: [{ chainId: toRpcChainId(chainId) }],
    });
  } catch (error) {
    if (!isUnknownChainError(error)) {
      throw error;
    }
    throw new Error(`Wallet does not know chain ${chainId}; add it manually before continuing`);
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

function shortAddress(address: string): string {
  return `${address.slice(0, 6)}...${address.slice(-4)}`;
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
