import { useEffect, useState, useCallback } from 'react';

import {
  MultisigClient,
  FalconSigner,
  type Multisig,
  type StateObject,
  type MultisigConfig,
} from '@openzeppelin/multisig-client';

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

// In-memory key storage (not persisted across page reloads)
interface KeyInfo {
  id: string;
  name: string;
  commitment: string;
  secretKey: SecretKey;
}

export default function App() {
  // Connection state
  const [psmUrl, setPsmUrl] = useState<string>(DEFAULT_PSM_URL);
  const [psmStatus, setPsmStatus] = useState<string>('Connecting...');
  const [psmPubkey, setPsmPubkey] = useState<string>('');
  const [clientReady, setClientReady] = useState<boolean>(false);
  const [webClient, setWebClient] = useState<WebClient | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Key state (in-memory only)
  const [keys, setKeys] = useState<KeyInfo[]>([]);
  const [newKeyName, setNewKeyName] = useState<string>('');
  const [generatingKey, setGeneratingKey] = useState<boolean>(false);

  // External commitments (for keys not in this browser)
  interface ExternalCommitment {
    id: string;
    name: string;
    commitment: string;
  }
  const [externalCommitments, setExternalCommitments] = useState<ExternalCommitment[]>([]);
  const [externalCommitmentInput, setExternalCommitmentInput] = useState<string>('');

  // Multisig creation state
  const [threshold, setThreshold] = useState<number>(1);
  const [selectedKeyIds, setSelectedKeyIds] = useState<string[]>([]);
  const [selectedExternalIds, setSelectedExternalIds] = useState<string[]>([]);
  const [creating, setCreating] = useState<boolean>(false);
  // Multisig state
  const [multisig, setMultisig] = useState<Multisig | null>(null);
  const [multisigClient, setMultisigClient] = useState<MultisigClient | null>(null);

  // PSM sync state
  const [configuredOnPsm, setConfiguredOnPsm] = useState<boolean>(false);
  const [configuringPsm, setConfiguringPsm] = useState<boolean>(false);
  const [psmState, setPsmState] = useState<StateObject | null>(null);
  const [syncingState, setSyncingState] = useState<boolean>(false);

  // Selected signer key (used for creating multisig)
  const [selectedSignerKeyId, setSelectedSignerKeyId] = useState<string>('');

  // Connect to PSM server (creates MultisigClient when webClient is available)
  const connectToPsm = useCallback(async (url: string, client?: WebClient) => {
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
  }, [webClient]);

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

  // Generate a new key (dynamic, in-memory + miden-sdk IndexedDB)
  const handleGenerateKey = async () => {
    if (!clientReady || !webClient || !newKeyName.trim()) {
      setError('Please enter a name for the key');
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

      // Add to in-memory state
      const keyInfo: KeyInfo = {
        id: crypto.randomUUID(),
        name: newKeyName.trim(),
        commitment,
        secretKey,
      };

      setKeys((prev) => [...prev, keyInfo]);
      setNewKeyName('');
    } catch (err) {
      setError(`Failed to generate key: ${err instanceof Error ? err.message : 'Unknown error'}`);
    } finally {
      setGeneratingKey(false);
    }
  };

  // Delete a key (removes from in-memory state only)
  const handleDeleteKey = (keyId: string) => {
    if (confirm('Are you sure you want to remove this key? The key will remain in IndexedDB until you reset.')) {
      setKeys((prev) => prev.filter((k) => k.id !== keyId));
      setSelectedKeyIds((prev) => prev.filter((id) => id !== keyId));
      if (selectedSignerKeyId === keyId) {
        setSelectedSignerKeyId('');
      }
    }
  };

  // Toggle key selection for multisig
  const toggleKeySelection = (keyId: string) => {
    setSelectedKeyIds((prev) =>
      prev.includes(keyId) ? prev.filter((id) => id !== keyId) : [...prev, keyId]
    );
  };

  // Toggle external commitment selection
  const toggleExternalSelection = (extId: string) => {
    setSelectedExternalIds((prev) =>
      prev.includes(extId) ? prev.filter((id) => id !== extId) : [...prev, extId]
    );
  };

  // Add external commitment
  const handleAddExternalCommitment = () => {
    const trimmed = externalCommitmentInput.trim();
    if (!trimmed) {
      setError('Please enter a commitment');
      return;
    }
    if (!/^[0-9a-fA-F]{64}$/.test(trimmed)) {
      setError('Commitment must be a 64-character hex string');
      return;
    }

    const external: ExternalCommitment = {
      id: `ext-${Date.now()}`,
      name: `External: ${trimmed.slice(0, 8)}...`,
      commitment: trimmed,
    };

    setExternalCommitments((prev) => [...prev, external]);
    setSelectedExternalIds((prev) => [...prev, external.id]);
    setExternalCommitmentInput('');
    setError(null);
  };

  // Delete external commitment
  const handleDeleteExternal = (extId: string) => {
    setExternalCommitments((prev) => prev.filter((e) => e.id !== extId));
    setSelectedExternalIds((prev) => prev.filter((id) => id !== extId));
  };

  // Get all selected signer commitments
  const getSelectedCommitments = (): string[] => {
    const commitments: string[] = [];

    for (const keyId of selectedKeyIds) {
      const key = keys.find((k) => k.id === keyId);
      if (key) {
        commitments.push(key.commitment);
      }
    }

    for (const extId of selectedExternalIds) {
      const ext = externalCommitments.find((e) => e.id === extId);
      if (ext) {
        commitments.push(ext.commitment);
      }
    }

    return commitments;
  };

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

    if (!selectedSignerKeyId) {
      setError('Please select a signing key first');
      return;
    }

    const keyInfo = keys.find((k) => k.id === selectedSignerKeyId);
    if (!keyInfo) {
      setError('Selected key not found');
      return;
    }

    const commitments = getSelectedCommitments();

    if (commitments.length === 0) {
      setError('Please select at least one signer');
      return;
    }

    if (threshold < 1 || threshold > commitments.length) {
      setError(`Threshold must be between 1 and ${commitments.length}`);
      return;
    }

    setCreating(true);
    setError(null);

    try {
      const config: MultisigConfig = {
        threshold,
        signerCommitments: commitments,
        psmCommitment: psmPubkey,
        psmEnabled: true,
      };

      // Create signer from in-memory secret key
      const signer = new FalconSigner(keyInfo.secretKey);

      // Create multisig account using the new API
      const ms = await multisigClient.create(config, signer);
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
      // Use the Multisig's registerOnPsm method
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

  const selectedCount = selectedKeyIds.length + selectedExternalIds.length;

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

      {/* Keys Section */}
      <section className="keystore-section">
        <h2>Keys</h2>
        <p className="section-description">
          Generate Falcon keys for signing. Keys are stored in the Miden SDK&apos;s IndexedDB and kept in memory for signing.
        </p>

        <div className="key-generator">
          <input
            type="text"
            placeholder="Key name (e.g., 'Alice', 'My Key')"
            value={newKeyName}
            onChange={(e) => setNewKeyName(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleGenerateKey()}
            disabled={!clientReady || generatingKey}
          />
          <button
            onClick={handleGenerateKey}
            className="btn btn-primary"
            disabled={!clientReady || generatingKey || !newKeyName.trim()}
          >
            {generatingKey ? 'Generating...' : 'Generate Key'}
          </button>
        </div>

        {keys.length > 0 && (
          <div className="keys-list">
            <h3>Your Keys (in-memory)</h3>
            {keys.map((key) => (
              <div key={key.id} className="key-item">
                <div className="key-info">
                  <span className="key-name">{key.name}</span>
                  <code
                    className="key-commitment"
                    onClick={() => copyToClipboard(key.commitment)}
                    title="Click to copy commitment"
                  >
                    {key.commitment.slice(0, 16)}...{key.commitment.slice(-8)}
                  </code>
                </div>
                <div className="key-actions">
                  <button
                    className={`btn btn-small ${selectedKeyIds.includes(key.id) ? 'btn-selected' : ''}`}
                    onClick={() => toggleKeySelection(key.id)}
                  >
                    {selectedKeyIds.includes(key.id) ? 'Selected' : 'Select'}
                  </button>
                  <button
                    className="btn btn-small btn-danger"
                    onClick={() => handleDeleteKey(key.id)}
                  >
                    Remove
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}

        {keys.length === 0 && (
          <p className="no-keys">No keys yet. Generate one above to get started.</p>
        )}
      </section>

      {/* Multisig Creation Section */}
      <section className="multisig-section">
        <h2>Create Multisig Account</h2>

        <div className="multisig-config">
          <div className="config-row">
            <label>
              <span className="label">Threshold:</span>
              <input
                type="number"
                min="1"
                max={Math.max(1, selectedCount)}
                value={threshold}
                onChange={(e) => setThreshold(Math.max(1, parseInt(e.target.value) || 1))}
              />
              <span className="hint">of {selectedCount} signer(s)</span>
            </label>
          </div>

          <div className="signers-selection">
            <h3>Signers</h3>

            {/* Selected keys summary */}
            {selectedKeyIds.length > 0 && (
              <div className="selected-signers">
                <h4>Selected keys:</h4>
                <ul>
                  {selectedKeyIds.map((id) => {
                    const key = keys.find((k) => k.id === id);
                    return key ? <li key={id}>{key.name}</li> : null;
                  })}
                </ul>
              </div>
            )}

            {/* External commitments list */}
            {externalCommitments.length > 0 && (
              <div className="external-commitments-list">
                <h4>External commitments:</h4>
                {externalCommitments.map((ext) => (
                  <div key={ext.id} className="external-item">
                    <code>{ext.commitment.slice(0, 16)}...{ext.commitment.slice(-8)}</code>
                    <div className="external-actions">
                      <button
                        className={`btn btn-small ${selectedExternalIds.includes(ext.id) ? 'btn-selected' : ''}`}
                        onClick={() => toggleExternalSelection(ext.id)}
                      >
                        {selectedExternalIds.includes(ext.id) ? 'Selected' : 'Select'}
                      </button>
                      <button
                        className="btn btn-small btn-danger"
                        onClick={() => handleDeleteExternal(ext.id)}
                      >
                        Remove
                      </button>
                    </div>
                  </div>
                ))}
              </div>
            )}

            {/* Add external commitment */}
            <div className="external-commitment">
              <h4>Add external commitment:</h4>
              <div className="external-input">
                <input
                  type="text"
                  placeholder="64-char hex commitment"
                  value={externalCommitmentInput}
                  onChange={(e) => setExternalCommitmentInput(e.target.value)}
                />
                <button
                  className="btn btn-small"
                  onClick={handleAddExternalCommitment}
                  disabled={!externalCommitmentInput.trim()}
                >
                  Add
                </button>
              </div>
            </div>
          </div>

          {/* Signing key selector */}
          <div className="signer-key-selector">
            <label>
              <span className="label">Your Signing Key:</span>
              <select
                value={selectedSignerKeyId}
                onChange={(e) => setSelectedSignerKeyId(e.target.value)}
                disabled={keys.length === 0}
              >
                <option value="">Select your key...</option>
                {keys.filter(k => selectedKeyIds.includes(k.id)).map((key) => (
                  <option key={key.id} value={key.id}>
                    {key.name}
                  </option>
                ))}
              </select>
              <span className="hint">This key will sign PSM requests</span>
            </label>
          </div>

          <button
            onClick={handleCreateAccount}
            className="btn btn-primary btn-large"
            disabled={!multisigClient || !psmPubkey || creating || selectedCount === 0 || !selectedSignerKeyId}
          >
            {creating ? 'Creating Account...' : `Create ${threshold}-of-${selectedCount} Multisig`}
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
                <code>{multisig.threshold}-of-{multisig.signerCommitments.length}</code>
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
          <p className="section-description">
            Register your account on PSM and sync state.
          </p>

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
