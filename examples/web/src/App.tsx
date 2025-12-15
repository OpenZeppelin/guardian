import { useEffect, useState, useCallback } from 'react';

import {
  PsmHttpClient,
  type StateObject,
  type ConfigureRequest,
  createMultisigAccount,
  type MultisigConfig,
  generateKey,
  loadKeys,
  deleteKey,
  getKey,
  type KeyEntry,
  createSigner,
  clearIndexedDB,
} from '@openzeppelin/multisig-client';

import { WebClient } from '@demox-labs/miden-sdk';

const DEFAULT_PSM_URL = 'http://localhost:3000';

export default function App() {
  // Connection state
  const [psmUrl, setPsmUrl] = useState<string>(DEFAULT_PSM_URL);
  const [psmStatus, setPsmStatus] = useState<string>('Connecting...');
  const [psmPubkey, setPsmPubkey] = useState<string>('');
  const [clientReady, setClientReady] = useState<boolean>(false);
  const [webClient, setWebClient] = useState<WebClient | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Keystore state
  const [keys, setKeys] = useState<KeyEntry[]>([]);
  const [newKeyName, setNewKeyName] = useState<string>('');
  const [generatingKey, setGeneratingKey] = useState<boolean>(false);

  // Multisig creation state
  const [threshold, setThreshold] = useState<number>(1);
  const [selectedSigners, setSelectedSigners] = useState<string[]>([]);
  const [externalCommitment, setExternalCommitment] = useState<string>('');
  const [creating, setCreating] = useState<boolean>(false);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const [account, setAccount] = useState<any>(null);

  // PSM sync state
  const [psmClient] = useState<PsmHttpClient>(() => new PsmHttpClient(DEFAULT_PSM_URL));
  const [configuredOnPsm, setConfiguredOnPsm] = useState<boolean>(false);
  const [configuringPsm, setConfiguringPsm] = useState<boolean>(false);
  const [psmState, setPsmState] = useState<StateObject | null>(null);
  const [syncingState, setSyncingState] = useState<boolean>(false);
  const [selectedSignerKey, setSelectedSignerKey] = useState<string>('');

  // Refresh keys from storage
  const refreshKeys = useCallback(() => {
    setKeys(loadKeys());
  }, []);

  // Connect to PSM server
  const connectToPsm = useCallback(async (url: string) => {
    setPsmStatus('Connecting...');
    try {
      const client = new PsmHttpClient(url);
      const pubkey = await client.getPubkey();
      setPsmPubkey(pubkey);
      setPsmStatus('Connected');
      setError(null);
    } catch {
      setPsmStatus('Disconnected');
      setPsmPubkey('');
      // Don't show error on initial auto-connect failure, just set status
    }
  }, []);

  // Load Miden SDK and create WebClient
  const loadMidenClient = async () => {
    try {
      // Create WebClient
      const client = await WebClient.createClient('https://rpc.testnet.miden.io:443');
      await client.syncState();
      setWebClient(client);
      setClientReady(true);
    } catch (err) {
      console.error('[loadMidenClient] Error:', err);
      setError(
        `Failed to initialize Miden client: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    }
  };

  // Generate a new key
  const handleGenerateKey = () => {
    if (!clientReady || !newKeyName.trim()) {
      setError('Please enter a name for the key');
      return;
    }

    setGeneratingKey(true);
    setError(null);

    try {
      generateKey(newKeyName.trim());
      setNewKeyName('');
      refreshKeys();
    } catch (err) {
      setError(`Failed to generate key: ${err instanceof Error ? err.message : 'Unknown error'}`);
    } finally {
      setGeneratingKey(false);
    }
  };

  // Delete a key
  const handleDeleteKey = (keyId: string) => {
    if (confirm('Are you sure you want to delete this key? This cannot be undone.')) {
      deleteKey(keyId);
      setSelectedSigners((prev) => prev.filter((id) => id !== keyId));
      refreshKeys();
    }
  };

  // Toggle signer selection
  const toggleSigner = (keyId: string) => {
    setSelectedSigners((prev) =>
      prev.includes(keyId) ? prev.filter((id) => id !== keyId) : [...prev, keyId]
    );
  };

  // Add external commitment
  const handleAddExternalCommitment = () => {
    const trimmed = externalCommitment.trim();
    if (!trimmed) {
      setError('Please enter a commitment');
      return;
    }
    if (!/^[0-9a-fA-F]{64}$/.test(trimmed)) {
      setError('Commitment must be a 64-character hex string');
      return;
    }
    // Add as a "virtual" key entry for display
    const virtualEntry: KeyEntry = {
      id: `external-${Date.now()}`,
      name: `External: ${trimmed.slice(0, 8)}...`,
      commitment: trimmed,
      secretKeyBase64: '', // No secret key for external
      createdAt: Date.now(),
    };
    setKeys((prev) => [...prev, virtualEntry]);
    setSelectedSigners((prev) => [...prev, virtualEntry.id]);
    setExternalCommitment('');
    setError(null);
  };

  // Get all selected signer commitments (not including PSM which is separate)
  const getSelectedCommitments = (): string[] => {
    const commitments: string[] = [];

    for (const keyId of selectedSigners) {
      const key = keys.find((k) => k.id === keyId);
      if (key) {
        commitments.push(key.commitment);
      }
    }

    return commitments;
  };

  // Create multisig account
  const handleCreateAccount = async () => {
    if (!clientReady || !webClient) {
      setError('SDK or WebClient not initialized');
      return;
    }

    if (!psmPubkey) {
      setError('Please connect to PSM server first');
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

      const result = await createMultisigAccount(webClient, config);
      setAccount(result.account);
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
    if (!clientReady || !account || !psmPubkey) {
      setError('Account not created or PSM not connected');
      return;
    }

    // Need a signer key selected for authentication
    if (!selectedSignerKey) {
      setError('Please select a key to sign requests');
      return;
    }

    const keyEntry = getKey(selectedSignerKey);
    if (!keyEntry) {
      setError('Selected key not found');
      return;
    }

    setConfiguringPsm(true);
    setError(null);

    try {
      // Create signer for authentication
      const signer = createSigner(keyEntry);
      psmClient.setSigner(signer);

      // Get all cosigner commitments (same as used in account creation)
      const commitments = getSelectedCommitments();

      // Build initial state JSON (account serialization)
      // For now, we use a simple placeholder - in production this would be the serialized account
      const accountId = account.id().toString();
      const initialStateData = JSON.stringify({
        account_id: accountId,
        nonce: account.nonce().toString(),
        // Add other account state as needed
      });

      const request: ConfigureRequest = {
        account_id: accountId,
        auth: {
          MidenFalconRpo: {
            cosigner_commitments: commitments,
          },
        },
        initial_state: {
          data: btoa(initialStateData), // Base64 encode
          account_id: accountId,
        },
        storage_type: 'Filesystem',
      };

      const response = await psmClient.configure(request);
      if (response.success) {
        setConfiguredOnPsm(true);
      } else {
        setError(`PSM configuration failed: ${response.message}`);
      }
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
    if (!clientReady || !account) {
      setError('Account not created');
      return;
    }

    if (!selectedSignerKey) {
      setError('Please select a key to sign requests');
      return;
    }

    const keyEntry = getKey(selectedSignerKey);
    if (!keyEntry) {
      setError('Selected key not found');
      return;
    }

    setSyncingState(true);
    setError(null);

    try {
      // Create signer for authentication
      const signer = createSigner(keyEntry);
      psmClient.setSigner(signer);

      const accountId = account.id().toString();
      const state = await psmClient.getState(accountId);
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
    refreshKeys();
    connectToPsm(psmUrl);
  }, [refreshKeys, connectToPsm, psmUrl]);

  const selectedCount = selectedSigners.length;

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

      {/* Keystore Section */}
      <section className="keystore-section">
        <h2>Keystore</h2>
        <p className="section-description">
          Generate Falcon keys for signing. Keys are stored in your browser&apos;s localStorage.
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
            <h3>Your Keys</h3>
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
                  {key.secretKeyBase64 && (
                    <button
                      className={`btn btn-small ${selectedSigners.includes(key.id) ? 'btn-selected' : ''}`}
                      onClick={() => toggleSigner(key.id)}
                    >
                      {selectedSigners.includes(key.id) ? '✓ Selected' : 'Select'}
                    </button>
                  )}
                  <button
                    className="btn btn-small btn-danger"
                    onClick={() => handleDeleteKey(key.id)}
                  >
                    Delete
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
            {selectedSigners.length > 0 && (
              <div className="selected-signers">
                <h4>Selected from keystore:</h4>
                <ul>
                  {selectedSigners.map((id) => {
                    const key = keys.find((k) => k.id === id);
                    return key ? <li key={id}>{key.name}</li> : null;
                  })}
                </ul>
              </div>
            )}

            {/* Add external commitment */}
            <div className="external-commitment">
              <h4>Add external commitment:</h4>
              <div className="external-input">
                <input
                  type="text"
                  placeholder="64-char hex commitment"
                  value={externalCommitment}
                  onChange={(e) => setExternalCommitment(e.target.value)}
                />
                <button
                  className="btn btn-small"
                  onClick={handleAddExternalCommitment}
                  disabled={!externalCommitment.trim()}
                >
                  Add
                </button>
              </div>
            </div>
          </div>

          <button
            onClick={handleCreateAccount}
            className="btn btn-primary btn-large"
            disabled={!webClient || !psmPubkey || creating || selectedCount === 0}
          >
            {creating ? 'Creating Account...' : `Create ${threshold}-of-${selectedCount} Multisig`}
          </button>
        </div>

        {account && (
          <div className="account-info">
            <h3>Account Created!</h3>
            <div className="account-details">
              <div>
                <span className="label">Account ID:</span>
                <code onClick={() => copyToClipboard(account.id().toString())} title="Click to copy">
                  {account.id().toString()}
                </code>
              </div>
              <div>
                <span className="label">Nonce:</span>
                <code>{account.nonce().toString()}</code>
              </div>
              <div>
                <span className="label">Is Public:</span>
                <code>{account.isPublic() ? 'Yes' : 'No'}</code>
              </div>
            </div>
          </div>
        )}
      </section>

      {/* PSM Sync Section - only show after account is created */}
      {account && (
        <section className="psm-sync-section">
          <h2>PSM State Sync</h2>
          <p className="section-description">
            Register your account on PSM and sync state. Select a key to authenticate requests.
          </p>

          <div className="signer-key-selector">
            <label>
              <span className="label">Signing Key:</span>
              <select
                value={selectedSignerKey}
                onChange={(e) => setSelectedSignerKey(e.target.value)}
                disabled={keys.filter((k) => k.secretKeyBase64).length === 0}
              >
                <option value="">Select a key...</option>
                {keys
                  .filter((k) => k.secretKeyBase64) // Only show keys with secret keys
                  .map((key) => (
                    <option key={key.id} value={key.id}>
                      {key.name}
                    </option>
                  ))}
              </select>
            </label>
          </div>

          <div className="psm-actions">
            {!configuredOnPsm ? (
              <button
                onClick={handleConfigureOnPsm}
                className="btn btn-primary"
                disabled={configuringPsm || !selectedSignerKey || !psmPubkey}
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
              disabled={syncingState || !selectedSignerKey || !configuredOnPsm}
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
