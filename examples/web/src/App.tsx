import { useEffect, useState, useCallback } from 'react';

import {
  MultisigClient,
  FalconSigner,
  type Multisig,
  type MultisigConfig,
} from '@openzeppelin/miden-multisig-client';
import { type StateObject } from '@openzeppelin/psm-client';

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
  const [psmState, setPsmState] = useState<StateObject | null>(null);
  const [syncingState, setSyncingState] = useState<boolean>(false);

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

  // Load Miden SDK and create WebClient
  const loadMidenClient = async () => {
    try {
      const client = await WebClient.createClient('https://rpc.testnet.miden.io:443');
      await client.syncState();
      setWebClient(client);
      setClientReady(true);
      // Connect to PSM with the new webClient
      connectToPsm(psmUrl, client);
    } catch (err) {
      console.error('[loadMidenClient] Error:', err);
      setError(
        `Failed to initialize Miden client: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    }
  };

  // Generate this tab's signer key
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

  // Add another signer's commitment (from another signer)
  const handleAddOtherSigner = () => {
    let trimmed = otherCommitmentInput.trim();
    if (!trimmed) {
      setError('Please enter a commitment');
      return;
    }

    // Normalize: strip prefix for validation, then store with prefix for consistency
    const withoutPrefix = trimmed.startsWith('0x') || trimmed.startsWith('0X')
      ? trimmed.slice(2)
      : trimmed;

    if (!/^[0-9a-fA-F]{64}$/.test(withoutPrefix)) {
      setError('Commitment must be a 64-character hex string');
      return;
    }

    // Store with 0x prefix for consistency with signer.commitment
    const normalizedCommitment = `0x${withoutPrefix.toLowerCase()}`;

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

  // Remove another signer
  const handleRemoveOtherSigner = (id: string) => {
    setOtherSigners((prev) => prev.filter((s) => s.id !== id));
  };

  // Get total signer count
  const totalSigners = (signer ? 1 : 0) + otherSigners.length;

  // Create multisig account
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
      // Collect all signer commitments (this tab + others)
      const signerCommitments = [signer.commitment, ...otherSigners.map((s) => s.commitment)];

      const config: MultisigConfig = {
        threshold,
        signerCommitments,
        psmCommitment: psmPubkey,
        psmEnabled: true,
      };

      // Create FalconSigner from this tab's secret key
      const falconSigner = new FalconSigner(signer.secretKey);

      // Create multisig account
      const ms = await multisigClient.create(config, falconSigner);
      setMultisig(ms);
    } catch (err) {
      console.error('Error creating account:', err);
      const message = err instanceof Error ? err.message : String(err);
      setError(`Failed to create account: ${message}`);
    } finally {
      setCreating(false);
    }
  };

  // Configure account on PSM server
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
      console.error('Error configuring on PSM:', err);
      const message = err instanceof Error ? err.message : String(err);
      setError(`Failed to configure on PSM: ${message}`);
    } finally {
      setConfiguringPsm(false);
    }
  };

  // Sync state from PSM server
  const handleSyncState = async () => {
    if (!clientReady || !multisig || !multisigClient) {
      setError('Account not created');
      return;
    }

    setSyncingState(true);
    setError(null);

    try {
      const state = await multisigClient.psmClient.getState(multisig.accountId);
      setPsmState(state);
    } catch (err) {
      console.error('Error syncing state:', err);
      const message = err instanceof Error ? err.message : String(err);
      setError(`Failed to sync state: ${message}`);
    } finally {
      setSyncingState(false);
    }
  };

  // Copy text to clipboard
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

      {/* Other Signers Section */}
      <section className="other-signers-section">
        <h2>Other Signers</h2>
        <p className="section-description">
          Add commitments from other signers to include them in the multisig.
        </p>

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

        {otherSigners.length > 0 && (
          <div className="other-signers-list">
            <h4>Added Signers ({otherSigners.length}):</h4>
            {otherSigners.map((s, index) => (
              <div key={s.id} className="other-signer-item">
                <span className="signer-number">Signer {index + 2}</span>
                <code onClick={() => copyToClipboard(s.commitment)} title="Click to copy">
                  {s.commitment.slice(0, 16)}...{s.commitment.slice(-8)}
                </code>
                <button className="btn btn-small btn-danger" onClick={() => handleRemoveOtherSigner(s.id)}>
                  Remove
                </button>
              </div>
            ))}
          </div>
        )}

        {otherSigners.length === 0 && (
          <p className="no-signers">No other signers added yet.</p>
        )}
      </section>

      {/* Multisig Creation Section */}
      <section className="multisig-section">
        <h2>Create Multisig Account</h2>

        <div className="multisig-config">
          <div className="signers-summary">
            <h4>Signers ({totalSigners} total):</h4>
            <ul>
              {signer && (
                <li>
                  <strong>You</strong>: {signer.commitment.slice(0, 8)}...
                </li>
              )}
              {otherSigners.map((s, index) => (
                <li key={s.id}>
                  Signer {index + 2}: {s.commitment.slice(0, 8)}...
                </li>
              ))}
            </ul>
          </div>

          <div className="config-row">
            <label>
              <span className="label">Threshold:</span>
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

          <button
            onClick={handleCreateAccount}
            className="btn btn-primary btn-large"
            disabled={!multisigClient || !psmPubkey || creating || !signer || totalSigners === 0}
          >
            {creating ? 'Creating Account...' : `Create ${threshold}-of-${totalSigners} Multisig`}
          </button>
        </div>

        {multisig && (
          <div className="account-info">
            <h3>Account Created!</h3>
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
        )}
      </section>

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
                  <code onClick={() => copyToClipboard(psmState.account_id)} title="Click to copy">
                    {psmState.account_id}
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
                  <code>{new Date(psmState.updated_at).toLocaleString()}</code>
                </div>
              </div>
            </div>
          )}
        </section>
      )}
    </div>
  );
}
