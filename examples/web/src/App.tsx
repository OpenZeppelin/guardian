import { useEffect, useState, useCallback } from 'react';

import {
  MultisigClient,
  FalconSigner,
  AccountInspector,
  type Multisig,
  type MultisigConfig,
  type AccountState,
  type DetectedMultisigConfig,
  type Proposal,
} from '@openzeppelin/miden-multisig-client';

import { WebClient, SecretKey } from '@demox-labs/miden-sdk';

// Clear all IndexedDB databases (resets miden-sdk state)
async function clearIndexedDB(): Promise<void> {
  const databases = await indexedDB.databases();
  const deletePromises = databases
    .filter((db) => db.name)
    .map(
      (db) =>
        new Promise<void>((resolve, reject) => {
          const request = indexedDB.deleteDatabase(db.name!);
          request.onsuccess = () => resolve();
          request.onerror = () => reject(request.error);
          request.onblocked = () => resolve();
        })
    );
  await Promise.all(deletePromises);
}

const DEFAULT_PSM_URL = 'http://localhost:3000';

// This tab's signer info
interface SignerInfo {
  commitment: string;
  secretKey: SecretKey;
}

// Other signers (from other tabs)
interface OtherSigner {
  id: string;
  commitment: string;
}

export default function App() {
  // Connection state
  const [psmUrl, setPsmUrl] = useState<string>(DEFAULT_PSM_URL);
  const [psmStatus, setPsmStatus] = useState<string>('Connecting...');
  const [psmPubkey, setPsmPubkey] = useState<string>('');
  const [clientReady, setClientReady] = useState<boolean>(false);
  const [webClient, setWebClient] = useState<WebClient | null>(null);
  const [error, setError] = useState<string | null>(null);

  // This tab's signer (single key per tab)
  const [signer, setSigner] = useState<SignerInfo | null>(null);
  const [generatingKey, setGeneratingKey] = useState<boolean>(false);

  // Other signers' commitments (from other tabs)
  const [otherSigners, setOtherSigners] = useState<OtherSigner[]>([]);
  const [otherCommitmentInput, setOtherCommitmentInput] = useState<string>('');

  // Multisig creation state
  const [threshold, setThreshold] = useState<number>(1);
  const [creating, setCreating] = useState<boolean>(false);

  // Multisig state
  const [multisig, setMultisig] = useState<Multisig | null>(null);
  const [multisigClient, setMultisigClient] = useState<MultisigClient | null>(null);

  // PSM sync state
  const [configuredOnPsm, setConfiguredOnPsm] = useState<boolean>(false);
  const [configuringPsm, setConfiguringPsm] = useState<boolean>(false);
  const [psmState, setPsmState] = useState<AccountState | null>(null);
  const [syncingState, setSyncingState] = useState<boolean>(false);

  // Proposals
  const [proposals, setProposals] = useState<Proposal[]>([]);
  const [newSignerCommitment, setNewSignerCommitment] = useState<string>('');
  const [creatingProposal, setCreatingProposal] = useState<boolean>(false);
  const [signingProposal, setSigningProposal] = useState<string | null>(null);
  const [executingProposal, setExecutingProposal] = useState<string | null>(null);

  // Load existing account state
  const [loadAccountIdInput, setLoadAccountIdInput] = useState<string>('');
  const [loadingAccount, setLoadingAccount] = useState<boolean>(false);
  const [detectedConfig, setDetectedConfig] = useState<DetectedMultisigConfig | null>(null);

  // Connect to PSM server (creates MultisigClient when webClient is available)
  const connectToPsm = useCallback(
    async (url: string, client?: WebClient) => {
      setPsmStatus('Connecting...');
      try {
        const wc = client ?? webClient;
        if (wc) {
          const msClient = new MultisigClient(wc, { psmEndpoint: url });
          const pubkey = await msClient.psmClient.getPubkey();
          setPsmPubkey(pubkey);
          setMultisigClient(msClient);
        } else {
          // Fetch pubkey directly without full client if webClient not ready
          const response = await fetch(`${url}/pubkey`);
          const data = await response.json();
          setPsmPubkey(data.commitment || '');
        }
        setPsmStatus('Connected');
        setError(null);
      } catch {
        setPsmStatus('Disconnected');
        setPsmPubkey('');
      }
    },
    [webClient]
  );

  const loadMidenClient = async () => {
    try {
      const client = await WebClient.createClient('https://rpc.testnet.miden.io:443');
      await client.syncState();
      setWebClient(client);
      setClientReady(true);
      connectToPsm(psmUrl, client);
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : 'Unknown error';
      setError(`Failed to initialize Miden client: ${errorMessage}`);
    }
  };

  const handleGenerateSigner = async () => {
    if (!clientReady || !webClient) {
      setError('Miden client not initialized');
      return;
    }

    setGeneratingKey(true);
    setError(null);

    try {
      // Generate a random seed
      const seed = new Uint8Array(32);
      crypto.getRandomValues(seed);

      // Create the Falcon secret key
      const secretKey = SecretKey.rpoFalconWithRNG(seed);

      // Store in miden-sdk's IndexedDB keystore
      await webClient.addAccountSecretKeyToWebStore(secretKey);

      // Get commitment for display
      const publicKey = secretKey.publicKey();
      const commitment = publicKey.toCommitment().toHex();

      setSigner({
        commitment,
        secretKey,
      });
    } catch (err) {
      setError(`Failed to generate key: ${err instanceof Error ? err.message : 'Unknown error'}`);
    } finally {
      setGeneratingKey(false);
    }
  };

  const normalizeCommitment = (hex: string) => {
    const trimmed = hex.trim();
    if (!trimmed) throw new Error('Commitment is required');
    const withoutPrefix =
      trimmed.startsWith('0x') || trimmed.startsWith('0X') ? trimmed.slice(2) : trimmed;
    if (!/^[0-9a-fA-F]{64}$/.test(withoutPrefix)) {
      throw new Error('Commitment must be a 64-character hex string');
    }
    return `0x${withoutPrefix.toLowerCase()}`;
  };

  // Add another signer's commitment (from another signer)
  const handleAddOtherSigner = () => {
    let normalizedCommitment: string;
    try {
      normalizedCommitment = normalizeCommitment(otherCommitmentInput);
    } catch (e: any) {
      setError(e?.message ?? 'Invalid commitment');
      return;
    }

    // Check if it's this signer's own commitment
    const ownCommitment = signer?.commitment.toLowerCase();
    if (ownCommitment && normalizedCommitment === ownCommitment) {
      setError("That's your own commitment");
      return;
    }

    // Check for duplicates
    if (otherSigners.some((s) => s.commitment.toLowerCase() === normalizedCommitment)) {
      setError('This commitment has already been added');
      return;
    }

    setOtherSigners((prev) => [...prev, { id: `other-${Date.now()}`, commitment: normalizedCommitment }]);
    setOtherCommitmentInput('');
    setError(null);
  };

  // Create "add signer" proposal
  const handleCreateAddSignerProposal = async () => {
    if (!multisig || !webClient) {
      setError('Multisig not created/loaded');
      return;
    }
    let commitment: string;
    try {
      commitment = normalizeCommitment(newSignerCommitment);
    } catch (e: any) {
      setError(e?.message ?? 'Invalid commitment');
      return;
    }

    setCreatingProposal(true);
    setError(null);
    try {
      const proposal = await multisig.createAddSignerProposal(webClient, commitment);
      // Refresh proposals list
      const synced = await multisig.syncProposals();
      setProposals(synced);
      setNewSignerCommitment('');
      if (!synced.find((p) => p.id === proposal.id)) {
        setProposals([...synced, proposal]);
      }
    } catch (err: any) {
      setError(err?.message ?? 'Failed to create proposal');
    } finally {
      setCreatingProposal(false);
    }
  };

  const handleSyncProposals = async () => {
    if (!multisig) {
      setError('Multisig not created/loaded');
      return;
    }
    setSyncingState(true);
    try {
      const synced = await multisig.syncProposals();
      setProposals(synced);
      setError(null);
    } catch (err: any) {
      setError(err?.message ?? 'Failed to sync proposals');
    } finally {
      setSyncingState(false);
    }
  };

  const handleSignProposal = async (proposalId: string) => {
    if (!multisig) {
      setError('Multisig not created/loaded');
      return;
    }
    setSigningProposal(proposalId);
    setError(null);
    try {
      await multisig.signProposal(proposalId);
      const synced = await multisig.syncProposals();
      setProposals(synced);
    } catch (err: any) {
      setError(err?.message ?? 'Failed to sign proposal');
    } finally {
      setSigningProposal(null);
    }
  };

  const handleExecuteProposal = async (proposalId: string) => {
    if (!multisig || !webClient) {
      setError('Multisig not created/loaded or webClient not ready');
      return;
    }
    setExecutingProposal(proposalId);
    setError(null);
    try {
      await multisig.executeProposal(proposalId, webClient);
      const synced = await multisig.syncProposals();
      setProposals(synced);
    } catch (err: any) {
      setError(err?.message ?? 'Failed to execute proposal');
    } finally {
      setExecutingProposal(null);
    }
  };

  const hasUserSigned = (proposal: Proposal): boolean => {
    if (!signer) return false;
    return proposal.signatures.some(
      (sig) => sig.signerId.toLowerCase() === signer.commitment.toLowerCase()
    );
  };

  const handleRemoveOtherSigner = (id: string) => {
    setOtherSigners((prev) => prev.filter((s) => s.id !== id));
  };

  const totalSigners = (signer ? 1 : 0) + otherSigners.length;

  const handleCreateAccount = async () => {
    if (!clientReady || !multisigClient) {
      setError('Miden client not initialized');
      return;
    }

    if (!psmPubkey) {
      setError('Please connect to PSM server first');
      return;
    }

    if (!signer) {
      setError('Please generate your signer key first');
      return;
    }

    if (totalSigners === 0) {
      setError('At least one signer is required');
      return;
    }

    if (threshold < 1 || threshold > totalSigners) {
      setError(`Threshold must be between 1 and ${totalSigners}`);
      return;
    }

    setCreating(true);
    setError(null);

    try {
      const signerCommitments = [signer.commitment, ...otherSigners.map((s) => s.commitment)];
      const config: MultisigConfig = {
        threshold,
        signerCommitments,
        psmCommitment: psmPubkey,
        psmEnabled: true,
      };
      const falconSigner = new FalconSigner(signer.secretKey);
      const ms = await multisigClient.create(config, falconSigner);
      setMultisig(ms);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setError(`Failed to create account: ${message}`);
    } finally {
      setCreating(false);
    }
  };

  const handleConfigureOnPsm = async () => {
    if (!clientReady || !multisig || !psmPubkey) {
      setError('Account not created or PSM not connected');
      return;
    }

    setConfiguringPsm(true);
    setError(null);

    try {
      await multisig.registerOnPsm();
      setConfiguredOnPsm(true);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setError(`Failed to configure on PSM: ${message}`);
    } finally {
      setConfiguringPsm(false);
    }
  };

  const handleLoadFromPsm = async () => {
    if (!clientReady || !multisigClient) {
      setError('Miden client not initialized');
      return;
    }

    if (!signer) {
      setError('Please generate your signer key first');
      return;
    }

    let accountId = loadAccountIdInput.trim();
    if (!accountId) {
      setError('Please enter an account ID');
      return;
    }

    if (!accountId.startsWith('0x')) {
      accountId = `0x${accountId}`;
    }

    setLoadingAccount(true);
    setError(null);
    setDetectedConfig(null);

    try {
      const falconSigner = new FalconSigner(signer.secretKey);

      // Use temporary config to fetch state for inspection
      const tempConfig: MultisigConfig = {
        threshold: 1,
        signerCommitments: [signer.commitment],
        psmCommitment: psmPubkey,
        psmEnabled: true,
      };

      const tempMs = await multisigClient.load(accountId, tempConfig, falconSigner);
      const state = await tempMs.fetchState();

      const detected = AccountInspector.fromBase64(state.stateDataBase64);
      setDetectedConfig(detected);

      const config: MultisigConfig = {
        threshold: detected.threshold,
        signerCommitments: detected.signerCommitments,
        psmCommitment: detected.psmCommitment || psmPubkey,
        psmEnabled: detected.psmEnabled,
      };

      const ms = await multisigClient.load(accountId, config, falconSigner);
      setMultisig(ms);
      setConfiguredOnPsm(true);
      setPsmState(state);
      setThreshold(detected.threshold);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (message.includes('404') || message.includes('not found')) {
        setError(`Account not found on PSM. Make sure the account has been registered first.`);
      } else {
        setError(`Failed to load account: ${message}`);
      }
    } finally {
      setLoadingAccount(false);
    }
  };

  const handleSyncState = async () => {
    if (!clientReady || !multisig) {
      setError('Account not created');
      return;
    }

    setSyncingState(true);
    setError(null);

    try {
      const state = await multisig.fetchState();
      setPsmState(state);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setError(`Failed to sync state: ${message}`);
    } finally {
      setSyncingState(false);
    }
  };

  const copyToClipboard = (text: string) => {
    navigator.clipboard.writeText(text);
  };

  useEffect(() => {
    loadMidenClient();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="app">
      <h1>Miden Multisig Web Example</h1>

      {/* Status Section */}
      <section className="status-section">
        <h2>Status</h2>
        <div className="status-grid">
          <div className="status-item">
            <span className="label">Miden Client:</span>
            <span className={`value ${clientReady ? 'ok' : 'loading'}`}>
              {clientReady ? 'Ready' : 'Initializing...'}
            </span>
          </div>
          <div className="status-item">
            <span className="label">PSM Server:</span>
            <span className={`value ${psmStatus === 'Connected' ? 'ok' : 'error'}`}>
              {psmStatus}
            </span>
          </div>
        </div>

        <div className="psm-url-config">
          <label>
            <span className="label">PSM URL:</span>
            <input
              type="text"
              value={psmUrl}
              onChange={(e) => setPsmUrl(e.target.value)}
              placeholder="http://localhost:3000"
            />
          </label>
          <button onClick={() => connectToPsm(psmUrl)} className="btn">
            Reconnect
          </button>
        </div>

        <div className="button-row">
          <button
            onClick={async () => {
              await clearIndexedDB();
              window.location.reload();
            }}
            className="btn btn-secondary"
          >
            Reset IndexedDB
          </button>
        </div>

        {psmPubkey && (
          <div className="pubkey">
            <span className="label">PSM Public Key:</span>
            <code onClick={() => copyToClipboard(psmPubkey)} title="Click to copy">
              {psmPubkey.slice(0, 16)}...{psmPubkey.slice(-8)}
            </code>
          </div>
        )}

        {error && <div className="error">{error}</div>}
      </section>

      {/* Your Signer Section */}
      <section className="signer-section">
        <h2>Your Signer</h2>
        <p className="section-description">
          Generate a unique signing key. Share your commitment with other signers to create a
          multisig.
        </p>

        {!signer ? (
          <div className="signer-generator">
            <button
              onClick={handleGenerateSigner}
              className="btn btn-primary"
              disabled={!clientReady || generatingKey}
            >
              {generatingKey ? 'Generating...' : 'Generate Signer Key'}
            </button>
          </div>
        ) : (
          <div className="signer-info">
            <div className="signer-details">
              <div>
                <span className="label">Your Commitment:</span>
                <code
                  onClick={() => copyToClipboard(signer.commitment)}
                  title="Click to copy - share with other signers"
                  className="copyable"
                >
                  {signer.commitment}
                </code>
              </div>
            </div>
            <p className="hint">Copy your commitment above and share it with other signers.</p>
          </div>
        )}
      </section>

      {/* Create Multisig Account Section */}
      {!multisig && (
        <section className="multisig-section">
          <h2>Create Multisig Account</h2>

          {/* Step 1: Signers */}
          <div className="multisig-step">
            <h3>1. Configure Signers</h3>
            <p className="section-description">
              Add commitments from other signers to include them in the multisig.
            </p>

            <div className="signers-summary">
              <h4>Signers ({totalSigners} total):</h4>
              <ul>
                {signer && (
                  <li>
                    <strong>You</strong>: <code>{signer.commitment.slice(0, 16)}...</code>
                  </li>
                )}
                {otherSigners.map((s, index) => (
                  <li key={s.id} className="other-signer-item">
                    <span>Signer {index + 2}: <code>{s.commitment.slice(0, 16)}...</code></span>
                    <button className="btn btn-small btn-danger" onClick={() => handleRemoveOtherSigner(s.id)}>
                      Remove
                    </button>
                  </li>
                ))}
              </ul>
            </div>

            <div className="add-signer">
              <input
                type="text"
                placeholder="Paste commitment from another signer (64-char hex)"
                value={otherCommitmentInput}
                onChange={(e) => setOtherCommitmentInput(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && handleAddOtherSigner()}
              />
              <button
                className="btn"
                onClick={handleAddOtherSigner}
                disabled={!otherCommitmentInput.trim()}
              >
                Add Signer
              </button>
            </div>
          </div>

          {/* Step 2: Threshold */}
          <div className="multisig-step">
            <h3>2. Set Threshold</h3>
            <div className="config-row">
              <label>
                <span className="label">Required signatures:</span>
                <input
                  type="number"
                  min="1"
                  max={Math.max(1, totalSigners)}
                  value={threshold}
                  onChange={(e) => setThreshold(Math.max(1, parseInt(e.target.value) || 1))}
                />
                <span className="hint">of {totalSigners} signer(s) required to approve</span>
              </label>
            </div>
          </div>

          {/* Step 3: Create */}
          <div className="multisig-step">
            <h3>3. Create Account</h3>
            <button
              onClick={handleCreateAccount}
              className="btn btn-primary btn-large"
              disabled={!multisigClient || !psmPubkey || creating || !signer || totalSigners === 0}
            >
              {creating ? 'Creating Account...' : `Create ${threshold}-of-${totalSigners} Multisig`}
            </button>
          </div>
        </section>
      )}

      {/* Load Existing Account Section */}
      {!multisig && (
        <section className="load-account-section">
          <h2>Or Load Existing Account</h2>
          <p className="section-description">
            Load an existing multisig account from PSM. The configuration will be
            automatically detected.
          </p>

          <div className="load-account-form">
            <input
              type="text"
              placeholder="Account ID (0x...)"
              value={loadAccountIdInput}
              onChange={(e) => setLoadAccountIdInput(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && handleLoadFromPsm()}
            />
            <button
              className="btn btn-primary"
              onClick={handleLoadFromPsm}
              disabled={!multisigClient || !signer || loadingAccount || !loadAccountIdInput.trim()}
            >
              {loadingAccount ? 'Loading...' : 'Load from PSM'}
            </button>
          </div>

          {detectedConfig && (
            <div className="detected-config">
              <h4>Detected Configuration:</h4>
              <div className="config-details">
                <div>
                  <span className="label">Type:</span>
                  <code>{detectedConfig.threshold}-of-{detectedConfig.numSigners} multisig</code>
                </div>
                <div>
                  <span className="label">PSM Enabled:</span>
                  <code>{detectedConfig.psmEnabled ? 'Yes' : 'No'}</code>
                </div>
                <div>
                  <span className="label">Signers:</span>
                  <ul>
                    {detectedConfig.signerCommitments.map((c, i) => (
                      <li key={i}>
                        <code>{c.slice(0, 16)}...{c.slice(-8)}</code>
                      </li>
                    ))}
                  </ul>
                </div>
              </div>
            </div>
          )}
        </section>
      )}

      {/* Account Info - shown after account is created/loaded */}
      {multisig && (
        <section className="account-section">
          <h2>Multisig Account</h2>
          <div className="account-info">
            <div className="account-details">
              <div>
                <span className="label">Account ID:</span>
                <code onClick={() => copyToClipboard(multisig.accountId)} title="Click to copy">
                  {multisig.accountId}
                </code>
              </div>
              <div>
                <span className="label">Threshold:</span>
                <code>
                  {multisig.threshold}-of-{multisig.signerCommitments.length}
                </code>
              </div>
              <div>
                <span className="label">Your Commitment:</span>
                <code onClick={() => copyToClipboard(multisig.signerCommitment)} title="Click to copy">
                  {multisig.signerCommitment.slice(0, 16)}...
                </code>
              </div>
            </div>
          </div>
        </section>
      )}

      {/* Proposals - add signer */}
      {multisig && (
        <section className="psm-sync-section">
          <h2>Add Signer Proposal</h2>
          <p className="section-description">
            Build a proposal that updates signer set and threshold by executing the multisig
            update_signers script to summary.
          </p>
          <div className="psm-actions">
            <input
              type="text"
              placeholder="New signer commitment (0x...)"
              value={newSignerCommitment}
              onChange={(e) => setNewSignerCommitment(e.target.value)}
              style={{ width: '100%', marginBottom: '8px' }}
            />
            <button onClick={handleCreateAddSignerProposal} disabled={creatingProposal || !webClient}>
              {creatingProposal ? 'Creating...' : 'Create proposal'}
            </button>
            <button onClick={handleSyncProposals} disabled={syncingState}>
              {syncingState ? 'Syncing...' : 'Sync proposals'}
            </button>
          </div>
          {proposals.length > 0 && (
            <div className="psm-state-info">
              <h3>Proposals ({proposals.length})</h3>
              <div className="proposals-list">
                {proposals.map((p) => {
                  const userSigned = hasUserSigned(p);
                  const canSign = p.status.type === 'pending' && !userSigned;
                  const canExecute =
                    p.status.type === 'ready' ||
                    (p.status.type === 'pending' && multisig && p.signatures.length >= multisig.threshold);
                  const isSigningThis = signingProposal === p.id;
                  const isExecutingThis = executingProposal === p.id;

                  return (
                    <div key={p.id} className="proposal-card">
                      <div className="proposal-header">
                        <code
                          className="proposal-id"
                          onClick={() => copyToClipboard(p.id)}
                          title="Click to copy full ID"
                        >
                          {p.id.slice(0, 20)}...
                        </code>
                        <span className={`status-badge status-${p.status.type}`}>
                          {p.status.type}
                        </span>
                      </div>

                      <div className="proposal-details">
                        <div>
                          <span className="label">Nonce:</span> {p.nonce}
                        </div>
                        <div>
                          <span className="label">Signatures:</span>{' '}
                          {p.signatures.length} / {multisig?.threshold ?? '?'}
                          {userSigned && <span className="signed-badge"> ✓ You signed</span>}
                        </div>
                      </div>

                      {p.signatures.length > 0 && (
                        <div className="proposal-signers">
                          <span className="label">Signers:</span>
                          <div className="signer-badges">
                            {p.signatures.map((sig) => (
                              <span
                                key={sig.signerId}
                                className={`signer-badge ${
                                  signer && sig.signerId.toLowerCase() === signer.commitment.toLowerCase()
                                    ? 'signer-badge-you'
                                    : ''
                                }`}
                                title={sig.signerId}
                              >
                                {sig.signerId.slice(0, 8)}...
                              </span>
                            ))}
                          </div>
                        </div>
                      )}

                      <div className="proposal-actions">
                        {canSign && (
                          <button
                            className="btn btn-primary"
                            onClick={() => handleSignProposal(p.id)}
                            disabled={isSigningThis || !!signingProposal}
                          >
                            {isSigningThis ? 'Signing...' : 'Sign'}
                          </button>
                        )}
                        {canExecute && (
                          <button
                            className="btn btn-success"
                            onClick={() => handleExecuteProposal(p.id)}
                            disabled={isExecutingThis || !!executingProposal}
                          >
                            {isExecutingThis ? 'Executing...' : 'Execute'}
                          </button>
                        )}
                        {!canSign && !canExecute && p.status.type === 'finalized' && (
                          <span className="finalized-badge">Finalized</span>
                        )}
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
          )}
        </section>
      )}

      {/* PSM Sync Section - only show after account is created */}
      {multisig && (
        <section className="psm-sync-section">
          <h2>PSM State Sync</h2>
          <p className="section-description">Register your account on PSM and sync state.</p>

          <div className="psm-actions">
            {!configuredOnPsm ? (
              <button
                onClick={handleConfigureOnPsm}
                className="btn btn-primary"
                disabled={configuringPsm || !psmPubkey}
              >
                {configuringPsm ? 'Registering...' : 'Register on PSM'}
              </button>
            ) : (
              <div className="psm-registered">
                <span className="success-badge">Registered on PSM</span>
              </div>
            )}

            <button
              onClick={handleSyncState}
              className="btn"
              disabled={syncingState || !configuredOnPsm}
            >
              {syncingState ? 'Syncing...' : 'Sync State'}
            </button>
          </div>

          {psmState && (
            <div className="psm-state-info">
              <h3>PSM State</h3>
              <div className="state-details">
                <div>
                  <span className="label">Account ID:</span>
                  <code onClick={() => copyToClipboard(psmState.accountId)} title="Click to copy">
                    {psmState.accountId}
                  </code>
                </div>
                <div>
                  <span className="label">Commitment:</span>
                  <code onClick={() => copyToClipboard(psmState.commitment)} title="Click to copy">
                    {psmState.commitment.slice(0, 16)}...{psmState.commitment.slice(-8)}
                  </code>
                </div>
                <div>
                  <span className="label">Updated:</span>
                  <code>{new Date(psmState.updatedAt).toLocaleString()}</code>
                </div>
              </div>
            </div>
          )}
        </section>
      )}
    </div>
  );
}
