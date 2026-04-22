import { useCallback, useEffect, useState } from 'react';
import { normalizeError } from '@multisig-browser/errors';
import {
  type CreateProposalInput,
  type InitSessionInput,
  useSmokeHarness,
} from './smokeHarness';
import {
  DEFAULT_BROWSER_LABEL,
  DEFAULT_GUARDIAN_ENDPOINT,
  DEFAULT_MIDEN_RPC_URL,
} from './config';

function parseCommitments(text: string): string[] {
  return text
    .split(/\r?\n|,/)
    .map((value) => value.trim())
    .filter(Boolean);
}

function formatJson(value: unknown): string {
  return JSON.stringify(value, null, 2);
}

function proposalPlaceholder(): string {
  return formatJson({
    type: 'add_signer',
    commitment: '0x',
    increaseThreshold: false,
  });
}

function bootBadge(snapshot: ReturnType<typeof useSmokeHarness>['snapshot']): {
  className: string;
  label: string;
} {
  switch (snapshot.bootStatus) {
    case 'initializing':
      return { className: 'warning', label: 'Booting' };
    case 'ready':
      return { className: 'success', label: 'Ready' };
    case 'error':
      return { className: 'error', label: 'Boot failed' };
    case 'idle':
      return { className: 'neutral', label: 'Idle' };
  }
}

export default function App() {
  const { api, snapshot, events, midenWalletConnectError, disconnectMidenWallet } =
    useSmokeHarness();
  const [sessionForm, setSessionForm] = useState<InitSessionInput>({
    guardianEndpoint: DEFAULT_GUARDIAN_ENDPOINT,
    midenRpcEndpoint: DEFAULT_MIDEN_RPC_URL,
    signerSource: 'local',
    signatureScheme: 'falcon',
    browserLabel: DEFAULT_BROWSER_LABEL,
  });
  const [threshold, setThreshold] = useState('2');
  const [otherCommitments, setOtherCommitments] = useState('');
  const [guardianCommitment, setGuardianCommitment] = useState('');
  const [procedureThresholdsJson, setProcedureThresholdsJson] = useState('[]');
  const [accountId, setAccountId] = useState('');
  const [stateDataBase64, setStateDataBase64] = useState('');
  const [proposalJson, setProposalJson] = useState(proposalPlaceholder);
  const [importJson, setImportJson] = useState('');
  const [exportedJson, setExportedJson] = useState('');
  const [lastResult, setLastResult] = useState('');
  const [uiError, setUiError] = useState<string | null>(null);
  const sessionReady = snapshot.initialized && snapshot.bootStatus === 'ready';
  const accountLoaded = Boolean(snapshot.multisig);
  const currentBootBadge = bootBadge(snapshot);

  useEffect(() => {
    setSessionForm({
      guardianEndpoint: snapshot.guardianEndpoint ?? DEFAULT_GUARDIAN_ENDPOINT,
      midenRpcEndpoint: snapshot.midenRpcEndpoint ?? DEFAULT_MIDEN_RPC_URL,
      signerSource: snapshot.signerSource ?? 'local',
      signatureScheme: snapshot.signatureScheme ?? 'falcon',
      browserLabel: snapshot.browserLabel ?? DEFAULT_BROWSER_LABEL,
    });
  }, [
    snapshot.browserLabel,
    snapshot.guardianEndpoint,
    snapshot.midenRpcEndpoint,
    snapshot.signatureScheme,
    snapshot.signerSource,
  ]);

  const runAction = useCallback(async (action: () => Promise<unknown>) => {
    setUiError(null);
    try {
      const result = await action();
      setLastResult(formatJson(result));
      return result;
    } catch (err) {
      setUiError(normalizeError(err));
      return null;
    }
  }, []);

  const handleInitSession = useCallback(async () => {
    await runAction(async () => api.initSession(sessionForm));
  }, [api, runAction, sessionForm]);

  const handleCreateAccount = useCallback(async () => {
    const parsedThresholds = procedureThresholdsJson.trim()
      ? JSON.parse(procedureThresholdsJson)
      : undefined;

    await runAction(async () =>
      api.createAccount({
        threshold: Number(threshold),
        otherCommitments: parseCommitments(otherCommitments),
        guardianCommitment: guardianCommitment.trim() || undefined,
        procedureThresholds: parsedThresholds,
      }),
    );
  }, [api, guardianCommitment, otherCommitments, procedureThresholdsJson, runAction, threshold]);

  const handleLoadAccount = useCallback(async () => {
    await runAction(async () => api.loadAccount({ accountId }));
  }, [accountId, api, runAction]);

  const handleRegisterOnGuardian = useCallback(async () => {
    await runAction(async () =>
      api.registerOnGuardian({
        stateDataBase64: stateDataBase64.trim() || undefined,
      }),
    );
  }, [api, runAction, stateDataBase64]);

  const handleCreateProposal = useCallback(async () => {
    const parsed = JSON.parse(proposalJson) as CreateProposalInput;
    await runAction(async () => api.createProposal(parsed));
  }, [api, proposalJson, runAction]);

  const handleImportProposal = useCallback(async () => {
    await runAction(async () => api.importProposal({ json: importJson }));
  }, [api, importJson, runAction]);

  return (
    <div className="app-shell">
      <header className="hero">
        <div>
          <p className="eyebrow">Browser Smoke Harness</p>
          <h1>`examples/smoke-web`</h1>
          <p className="hero-copy">
            Minimal browser surface for smoke-testing `@openzeppelin/miden-multisig-client`.
            Use one browser or browser profile per cosigner session.
          </p>
        </div>
        <div className="hero-callout">
          <code>window.smoke</code>
          <span>Primary automation interface</span>
        </div>
      </header>

      <main className="layout">
        <section className="panel">
          <div className="panel-header">
            <h2>Session</h2>
            <span className={`badge ${currentBootBadge.className}`}>
              {currentBootBadge.label}
            </span>
          </div>
          <div className="form-grid">
            <label>
              <span>Guardian endpoint</span>
              <input
                value={sessionForm.guardianEndpoint ?? ''}
                onChange={(event) =>
                  setSessionForm((current) => ({
                    ...current,
                    guardianEndpoint: event.target.value,
                  }))
                }
              />
            </label>
            <label>
              <span>Miden RPC endpoint</span>
              <input
                value={sessionForm.midenRpcEndpoint ?? ''}
                onChange={(event) =>
                  setSessionForm((current) => ({
                    ...current,
                    midenRpcEndpoint: event.target.value,
                  }))
                }
              />
            </label>
            <label>
              <span>Signer source</span>
              <select
                value={sessionForm.signerSource ?? 'local'}
                onChange={(event) =>
                  setSessionForm((current) => ({
                    ...current,
                    signerSource: event.target.value as InitSessionInput['signerSource'],
                  }))
                }
              >
                <option value="local">Local</option>
                <option value="para">Para</option>
                <option value="miden-wallet">Miden Wallet</option>
              </select>
            </label>
            <label>
              <span>Signature scheme</span>
              <select
                value={sessionForm.signatureScheme ?? 'falcon'}
                onChange={(event) =>
                  setSessionForm((current) => ({
                    ...current,
                    signatureScheme: event.target.value as InitSessionInput['signatureScheme'],
                  }))
                }
              >
                <option value="falcon">Falcon</option>
                <option value="ecdsa">ECDSA</option>
              </select>
            </label>
            <label className="wide">
              <span>Browser label</span>
              <input
                value={sessionForm.browserLabel ?? ''}
                onChange={(event) =>
                  setSessionForm((current) => ({
                    ...current,
                    browserLabel: event.target.value,
                  }))
                }
              />
            </label>
          </div>
          <div className="actions">
            <button onClick={handleInitSession}>Reinitialize session</button>
            <button onClick={() => runAction(async () => api.connectPara())}>Connect Para</button>
            <button onClick={() => runAction(async () => api.connectMidenWallet())}>
              Connect Miden Wallet
            </button>
            <button onClick={() => runAction(async () => disconnectMidenWallet())}>
              Disconnect Miden Wallet
            </button>
            <button onClick={() => runAction(async () => api.status())}>Status</button>
            <button onClick={() => runAction(async () => api.clearLocalState())}>
              Clear local state
            </button>
            <button onClick={() => window.location.reload()}>Fresh reload</button>
          </div>
          <p className="hint">
            The page boots once on load like `examples/web`. Separate browsers or browser profiles
            are the supported concurrent-cosigner model.
          </p>
        </section>

        <section className="panel">
          <div className="panel-header">
            <h2>Snapshot</h2>
            {snapshot.busyAction ? (
              <span className="badge warning">Running {snapshot.busyAction}</span>
            ) : null}
          </div>
          <div className="status-grid">
            <div>
              <span className="label">Browser</span>
              <strong>{snapshot.browserLabel ?? 'Unnamed session'}</strong>
            </div>
            <div>
              <span className="label">Boot status</span>
              <strong>{snapshot.bootStatus}</strong>
            </div>
            <div>
              <span className="label">Signer source</span>
              <strong>{snapshot.signerSource ?? 'n/a'}</strong>
            </div>
            <div>
              <span className="label">Scheme</span>
              <strong>{snapshot.signatureScheme ?? 'n/a'}</strong>
            </div>
            <div>
              <span className="label">Guardian pubkey</span>
              <strong>{snapshot.guardianPubkey ?? 'n/a'}</strong>
            </div>
            <div>
              <span className="label">Account ID</span>
              <strong>{snapshot.multisig?.accountId ?? 'n/a'}</strong>
            </div>
            <div>
              <span className="label">Threshold</span>
              <strong>
                {snapshot.multisig
                  ? `${snapshot.multisig.threshold}-of-${snapshot.multisig.signerCommitments.length}`
                  : 'n/a'}
              </strong>
            </div>
            <div>
              <span className="label">Local Falcon commitment</span>
              <strong>{snapshot.localSigners?.falconCommitment ?? 'n/a'}</strong>
            </div>
            <div>
              <span className="label">Local ECDSA commitment</span>
              <strong>{snapshot.localSigners?.ecdsaCommitment ?? 'n/a'}</strong>
            </div>
            <div>
              <span className="label">Para</span>
              <strong>{snapshot.para.connected ? snapshot.para.commitment : 'Disconnected'}</strong>
            </div>
            <div>
              <span className="label">Miden Wallet</span>
              <strong>
                {snapshot.midenWallet.connected
                  ? snapshot.midenWallet.commitment
                  : 'Disconnected'}
              </strong>
            </div>
          </div>
          {snapshot.bootError || snapshot.lastError || uiError || midenWalletConnectError ? (
            <div className="error-box">
              {uiError ?? snapshot.bootError ?? snapshot.lastError ?? midenWalletConnectError}
            </div>
          ) : null}
        </section>

        <section className="panel">
          <div className="panel-header">
            <h2>Account</h2>
          </div>
          <div className="form-grid">
            <label>
              <span>Threshold</span>
              <input value={threshold} onChange={(event) => setThreshold(event.target.value)} />
            </label>
            <label>
              <span>Guardian commitment</span>
              <input
                value={guardianCommitment}
                onChange={(event) => setGuardianCommitment(event.target.value)}
                placeholder="Optional, will be fetched if blank"
              />
            </label>
            <label className="wide">
              <span>Other signer commitments</span>
              <textarea
                value={otherCommitments}
                onChange={(event) => setOtherCommitments(event.target.value)}
                placeholder="One commitment per line"
              />
            </label>
            <label className="wide">
              <span>Procedure thresholds JSON</span>
              <textarea
                value={procedureThresholdsJson}
                onChange={(event) => setProcedureThresholdsJson(event.target.value)}
                placeholder='[{"procedure":"send_asset","threshold":1}]'
              />
            </label>
            <label className="wide">
              <span>Account ID</span>
              <input
                value={accountId}
                onChange={(event) => setAccountId(event.target.value)}
                placeholder="0x..."
              />
            </label>
            <label className="wide">
              <span>State data (optional for register)</span>
              <textarea
                value={stateDataBase64}
                onChange={(event) => setStateDataBase64(event.target.value)}
                placeholder="Base64-encoded serialized account"
              />
            </label>
          </div>
          <div className="actions">
            <button disabled={!sessionReady} onClick={handleCreateAccount}>
              Create account
            </button>
            <button disabled={!sessionReady} onClick={handleLoadAccount}>
              Load account
            </button>
            <button disabled={!sessionReady || !accountLoaded} onClick={handleRegisterOnGuardian}>
              Register on Guardian
            </button>
            <button
              disabled={!sessionReady || !accountLoaded}
              onClick={() => runAction(async () => api.sync())}
            >
              Sync
            </button>
            <button
              disabled={!sessionReady || !accountLoaded}
              onClick={() => runAction(async () => api.fetchState())}
            >
              Fetch state
            </button>
            <button
              disabled={!sessionReady || !accountLoaded}
              onClick={() => runAction(async () => api.verifyStateCommitment())}
            >
              Verify state commitment
            </button>
            <button
              disabled={!sessionReady || !accountLoaded}
              onClick={() => runAction(async () => api.listConsumableNotes())}
            >
              List notes
            </button>
          </div>
        </section>

        <section className="panel">
          <div className="panel-header">
            <h2>Proposals</h2>
            <span className="badge neutral">{snapshot.proposals.length} cached</span>
          </div>
          <div className="form-grid">
            <label className="wide">
              <span>Create proposal JSON</span>
              <textarea
                value={proposalJson}
                onChange={(event) => setProposalJson(event.target.value)}
              />
            </label>
            <label className="wide">
              <span>Import proposal JSON</span>
              <textarea
                value={importJson}
                onChange={(event) => setImportJson(event.target.value)}
              />
            </label>
          </div>
          <div className="actions">
            <button disabled={!sessionReady || !accountLoaded} onClick={handleCreateProposal}>
              Create proposal
            </button>
            <button
              disabled={!sessionReady || !accountLoaded}
              onClick={() => runAction(async () => api.listProposals())}
            >
              List proposals
            </button>
            <button disabled={!sessionReady || !accountLoaded} onClick={handleImportProposal}>
              Import proposal
            </button>
          </div>
          <div className="proposal-list">
            {snapshot.proposals.map((proposal) => (
              <article key={proposal.id} className="proposal-card">
                <div className="proposal-header">
                  <div>
                    <h3>{proposal.metadata.proposalType}</h3>
                    <p>{proposal.id}</p>
                  </div>
                  <span className={`badge ${proposal.status === 'ready' ? 'success' : 'neutral'}`}>
                    {proposal.status}
                  </span>
                </div>
                <pre>{formatJson(proposal.metadata)}</pre>
                <div className="actions">
                  <button
                    disabled={!sessionReady}
                    onClick={() =>
                      runAction(async () => api.signProposal({ proposalId: proposal.id }))
                    }
                  >
                    Sign
                  </button>
                  <button
                    disabled={!sessionReady}
                    onClick={() =>
                      runAction(async () => api.executeProposal({ proposalId: proposal.id }))
                    }
                  >
                    Execute
                  </button>
                  <button
                    disabled={!sessionReady}
                    onClick={() =>
                      runAction(async () => {
                        const result = await api.exportProposal({ proposalId: proposal.id });
                        setExportedJson(result.json);
                        return result;
                      })
                    }
                  >
                    Export
                  </button>
                  <button
                    disabled={!sessionReady}
                    onClick={() =>
                      runAction(async () => {
                        const result = await api.signProposalOffline({ proposalId: proposal.id });
                        setExportedJson(result.json);
                        return result;
                      })
                    }
                  >
                    Offline sign
                  </button>
                </div>
              </article>
            ))}
          </div>
        </section>

        <section className="panel two-column">
          <div>
            <div className="panel-header">
              <h2>Last Result</h2>
            </div>
            <pre className="result-box">{lastResult || 'No command executed yet.'}</pre>
          </div>
          <div>
            <div className="panel-header">
              <h2>Exported JSON</h2>
            </div>
            <textarea
              className="result-box textarea-box"
              value={exportedJson}
              onChange={(event) => setExportedJson(event.target.value)}
              placeholder="Exported or offline-signed proposal JSON appears here."
            />
          </div>
        </section>

        <section className="panel">
          <div className="panel-header">
            <h2>Events</h2>
            <button onClick={() => runAction(async () => api.events())}>Refresh event stream</button>
          </div>
          <div className="event-list">
            {events.map((eventEntry) => (
              <div key={eventEntry.id} className="event-row">
                <div>
                  <strong>{eventEntry.action}</strong>
                  <span>{eventEntry.timestamp}</span>
                </div>
                <div>
                  <span className={`badge ${eventEntry.outcome === 'succeeded' ? 'success' : 'error'}`}>
                    {eventEntry.outcome}
                  </span>
                  <span>{eventEntry.durationMs} ms</span>
                </div>
                <p>{eventEntry.error ?? 'ok'}</p>
              </div>
            ))}
          </div>
        </section>

        <section className="panel">
          <div className="panel-header">
            <h2>Console Examples</h2>
          </div>
          <pre className="result-box">
{`await window.smoke.status();

await window.smoke.initSession({
  guardianEndpoint: 'http://localhost:3000',
  midenRpcEndpoint: 'https://rpc.devnet.miden.io',
  signerSource: 'local',
  signatureScheme: 'falcon',
  browserLabel: 'chrome-a',
});

await window.smoke.createAccount({
  threshold: 2,
  otherCommitments: ['0x...'],
});

await window.smoke.createProposal({
  type: 'add_signer',
  commitment: '0x...',
  increaseThreshold: false,
});`}
          </pre>
        </section>
      </main>
    </div>
  );
}
