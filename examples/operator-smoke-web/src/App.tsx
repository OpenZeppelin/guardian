import { useEffect, useMemo, useState } from 'react';
import {
  GuardianOperatorHttpClient,
  GuardianOperatorHttpError,
  type DashboardAccountDetail,
  type DashboardAccountSummary,
  type OperatorChallenge,
  type VerifyOperatorResponse,
} from '@openzeppelin/guardian-operator-client';
import { DEFAULT_GUARDIAN_BASE_URL, DEV_GUARDIAN_TARGET } from './config';
import { useLocalFalconSigner } from './localSigner';

function formatJson(value: unknown): string {
  return JSON.stringify(value, null, 2);
}

function normalizeError(error: unknown): string {
  if (error instanceof GuardianOperatorHttpError) {
    return error.data?.error ?? error.message;
  }

  if (error instanceof Error) {
    return error.message;
  }

  if (typeof error === 'string') {
    return error;
  }

  return 'Unknown error';
}

function buildOperatorPublicKeysJson(publicKey: string): string {
  return formatJson([publicKey]);
}

function normalizeHex(value: string | null | undefined): string {
  return (value ?? '').trim().toLowerCase();
}

export default function App() {
  const { session, sessionError, generate, clear, signWordHex } = useLocalFalconSigner();
  const [guardianBaseUrl, setGuardianBaseUrl] = useState(DEFAULT_GUARDIAN_BASE_URL);
  const [operatorCommitment, setOperatorCommitment] = useState('');
  const [challenge, setChallenge] = useState<OperatorChallenge | null>(null);
  const [verifyResponse, setVerifyResponse] = useState<VerifyOperatorResponse | null>(null);
  const [accounts, setAccounts] = useState<DashboardAccountSummary[]>([]);
  const [accountId, setAccountId] = useState('');
  const [account, setAccount] = useState<DashboardAccountDetail | null>(null);
  const [lastResult, setLastResult] = useState('');
  const [uiError, setUiError] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [pagedLimit, setPagedLimit] = useState('2');
  const [pagedAccounts, setPagedAccounts] = useState<DashboardAccountSummary[]>([]);
  const [pagedCursor, setPagedCursor] = useState<string | null>(null);
  const [pagedPageCount, setPagedPageCount] = useState(0);
  const [globalDeltaStatusFilter, setGlobalDeltaStatusFilter] = useState<{
    candidate: boolean;
    canonical: boolean;
    discarded: boolean;
  }>({ candidate: false, canonical: false, discarded: false });

  const client = useMemo(
    () =>
      new GuardianOperatorHttpClient({
        baseUrl: guardianBaseUrl,
        credentials: 'include',
      }),
    [guardianBaseUrl],
  );

  const effectiveCommitment = operatorCommitment.trim() || session.commitment || '';
  const operatorPublicKeysJson = session.publicKey
    ? buildOperatorPublicKeysJson(session.publicKey)
    : '';
  const signerCommitmentMismatch =
    session.ready &&
    operatorCommitment.trim().length > 0 &&
    normalizeHex(operatorCommitment) !== normalizeHex(session.commitment);

  useEffect(() => {
    if (!session.commitment) {
      return;
    }

    setOperatorCommitment((current) => current || session.commitment || '');
  }, [session.commitment]);

  async function generateSigner() {
    await runAction('generateLocalSigner', async () => {
      const nextSession = await generate();
      setOperatorCommitment(nextSession.commitment ?? '');
      setChallenge(null);
      setVerifyResponse(null);
      setAccounts([]);
      setAccount(null);
      resetPagination();
      return nextSession;
    });
  }

  async function clearSigner() {
    await runAction('clearLocalSigner', async () => {
      const nextSession = await clear();
      setOperatorCommitment('');
      setChallenge(null);
      setVerifyResponse(null);
      setAccounts([]);
      setAccount(null);
      resetPagination();
      return nextSession;
    });
  }

  async function runAction<T>(label: string, action: () => Promise<T>): Promise<T | null> {
    setBusyAction(label);
    setUiError(null);

    try {
      const result = await action();
      setLastResult(formatJson(result));
      return result;
    } catch (error) {
      setUiError(normalizeError(error));
      return null;
    } finally {
      setBusyAction(null);
    }
  }

  async function requestChallenge() {
    await runAction('requestChallenge', async () => {
      if (!effectiveCommitment) {
        throw new Error('Operator commitment is required');
      }
      const response = await client.challenge(effectiveCommitment);
      setChallenge(response.challenge);
      return response;
    });
  }

  async function login() {
    await runAction('login', async () => {
      if (!effectiveCommitment) {
        throw new Error('Operator commitment is required');
      }
      if (!session.ready) {
        throw new Error('Generate a local Falcon signer first');
      }

      const challengeResponse = await client.challenge(effectiveCommitment);
      setChallenge(challengeResponse.challenge);
      const signature = await signWordHex(challengeResponse.challenge.signingDigest);
      const response = await client.verify({
        commitment: effectiveCommitment,
        signature,
      });
      setVerifyResponse(response);
      return response;
    });
  }

  async function listAccounts() {
    await runAction('listAccounts', async () => {
      const response = await client.listAccounts();
      setAccounts(response.items);
      return response;
    });
  }

  async function fetchAccount() {
    await runAction('getAccount', async () => {
      if (!accountId.trim()) {
        throw new Error('Account ID is required');
      }
      const detail = await client.getAccount(accountId.trim());
      setAccount(detail);
      return detail;
    });
  }

  async function dashboardInfo() {
    await runAction('dashboardInfo', () => client.getDashboardInfo());
  }

  async function listAccountDeltas() {
    await runAction('listAccountDeltas', () => {
      const id = accountId.trim();
      if (!id) throw new Error('Account ID is required');
      return client.listAccountDeltas(id);
    });
  }

  async function listAccountProposals() {
    await runAction('listAccountProposals', () => {
      const id = accountId.trim();
      if (!id) throw new Error('Account ID is required');
      return client.listAccountProposals(id);
    });
  }

  async function listGlobalDeltas() {
    await runAction('listGlobalDeltas', () => {
      const selected = (
        ['candidate', 'canonical', 'discarded'] as const
      ).filter((s) => globalDeltaStatusFilter[s]);
      return client.listGlobalDeltas(
        selected.length > 0 ? { status: selected } : {},
      );
    });
  }

  async function listGlobalProposals() {
    await runAction('listGlobalProposals', () => client.listGlobalProposals());
  }

  async function paginateAccounts() {
    await runAction('paginateAccounts', async () => {
      const firstPage = await client.listAccounts({ limit: 1 });
      const secondPage = firstPage.nextCursor
        ? await client.listAccounts({ limit: 1, cursor: firstPage.nextCursor })
        : null;
      return { firstPage, secondPage };
    });
  }

  function parsePagedLimit(): number {
    const parsed = Number.parseInt(pagedLimit, 10);
    if (!Number.isFinite(parsed) || parsed < 1) {
      throw new Error('Limit must be a positive integer');
    }
    return parsed;
  }

  async function loadFirstPage() {
    await runAction('loadFirstPage', async () => {
      const limit = parsePagedLimit();
      const page = await client.listAccounts({ limit });
      setPagedAccounts(page.items);
      setPagedCursor(page.nextCursor);
      setPagedPageCount(1);
      return page;
    });
  }

  async function loadMore() {
    await runAction('loadMore', async () => {
      if (!pagedCursor) {
        throw new Error('No more pages — nextCursor is null');
      }
      const limit = parsePagedLimit();
      const page = await client.listAccounts({ limit, cursor: pagedCursor });
      setPagedAccounts((prev) => [...prev, ...page.items]);
      setPagedCursor(page.nextCursor);
      setPagedPageCount((prev) => prev + 1);
      return page;
    });
  }

  function resetPagination() {
    setPagedAccounts([]);
    setPagedCursor(null);
    setPagedPageCount(0);
  }

  async function logout() {
    await runAction('logout', async () => {
      const response = await client.logout();
      setVerifyResponse(null);
      setAccounts([]);
      setAccount(null);
      resetPagination();
      setChallenge(null);
      return response;
    });
  }

  return (
    <div className="app-shell">
      <header className="hero">
        <div>
          <p className="eyebrow">Guardian Operator UI</p>
          <h1>Local Falcon Harness</h1>
          <p className="hero-copy">
            This page generates a Falcon key in the browser and drives the operator endpoints
            through <code>@openzeppelin/guardian-operator-client</code>.
          </p>
        </div>
        <div className="hero-callout">
          <span>Guardian target</span>
          <code>{DEV_GUARDIAN_TARGET}</code>
        </div>
      </header>

      <main className="layout">
        <section className="panel">
          <div className="panel-header">
            <h2>Session</h2>
            <span className={`badge ${verifyResponse ? 'success' : 'neutral'}`}>
              {verifyResponse ? 'Authenticated' : 'Logged out'}
            </span>
          </div>

          <div className="form-grid">
            <label>
              <span>Guardian base URL</span>
              <input
                value={guardianBaseUrl}
                onChange={(event) => setGuardianBaseUrl(event.target.value)}
              />
            </label>
            <label>
              <span>Operator commitment</span>
              <input
                value={operatorCommitment}
                onChange={(event) => setOperatorCommitment(event.target.value)}
                placeholder="0x..."
              />
            </label>
          </div>

          <div className="actions">
            <button onClick={() => void generateSigner()}>Generate local Falcon signer</button>
            <button onClick={() => void clearSigner()}>Clear signer</button>
            <button onClick={() => void requestChallenge()}>Request challenge</button>
            <button onClick={() => void login()}>Login</button>
            <button onClick={() => void listAccounts()}>List accounts</button>
            <button onClick={() => void paginateAccounts()}>Paginate accounts</button>
            <button onClick={() => void dashboardInfo()}>Dashboard info</button>
            <button onClick={() => void listGlobalDeltas()}>List global deltas</button>
            <button onClick={() => void listGlobalProposals()}>List global proposals</button>
          </div>

          <fieldset className="status-filter">
            <legend>Global delta status filter</legend>
            {(['candidate', 'canonical', 'discarded'] as const).map((status) => (
              <label key={status} className="inline-check">
                <input
                  type="checkbox"
                  checked={globalDeltaStatusFilter[status]}
                  onChange={(event) =>
                    setGlobalDeltaStatusFilter((prev) => ({
                      ...prev,
                      [status]: event.target.checked,
                    }))
                  }
                />{' '}
                {status}
              </label>
            ))}
            <p className="hint">
              No checkboxes = no filter (server default). One or more = comma-separated{' '}
              <code>status</code> param. Garbage values surface as <code>invalid_status_filter</code>{' '}
              from the server — try toggling via the URL to exercise that path.
            </p>
          </fieldset>

          <div className="actions">
            <button onClick={() => void logout()}>Logout</button>
          </div>

          {busyAction ? (
            <p className="hint">
              Busy: <code>{busyAction}</code>
            </p>
          ) : null}

          {sessionError ? (
            <div className="error-box">
              <strong>Signer error:</strong> {sessionError}
            </div>
          ) : null}
          {uiError ? (
            <div className="error-box">
              <strong>Action error:</strong> {uiError}
            </div>
          ) : null}
          {signerCommitmentMismatch ? (
            <div className="error-box">
              <strong>Commitment mismatch:</strong> The input commitment does not match the active
              local signer. Guardian will reject login until they match.
            </div>
          ) : null}
        </section>

        <section className="panel">
          <div className="panel-header">
            <h2>Signer</h2>
            <span className={`badge ${session.ready ? 'success' : 'neutral'}`}>
              {session.ready ? 'Ready' : 'Missing'}
            </span>
          </div>

          <div className="status-grid">
            <div>
              <span className="label">Public key length</span>
              <strong>{session.publicKeyLength ?? 'n/a'}</strong>
            </div>
            <div>
              <span className="label">Public key</span>
              <strong>{session.publicKey ?? 'n/a'}</strong>
            </div>
            <div>
              <span className="label">Commitment</span>
              <strong>{session.commitment ?? 'n/a'}</strong>
            </div>
            <div>
              <span className="label">Persisted</span>
              <strong>{session.persisted ? 'yes' : 'no'}</strong>
            </div>
            <div>
              <span className="label">Operator ID</span>
              <strong>{verifyResponse?.operatorId ?? 'n/a'}</strong>
            </div>
            <div>
              <span className="label">Session expiry</span>
              <strong>{verifyResponse?.expiresAt ?? 'n/a'}</strong>
            </div>
          </div>
        </section>

        <section className="two-column">
          <section className="panel">
            <div className="panel-header">
              <h2>Operator Public Keys JSON</h2>
            </div>
            {operatorPublicKeysJson ? (
              <pre className="result-box">{operatorPublicKeysJson}</pre>
            ) : (
              <p className="hint">
                Generate a local signer first to get the operator public keys JSON.
              </p>
            )}
          </section>

          <section className="panel">
            <div className="panel-header">
              <h2>Challenge</h2>
            </div>
            {challenge ? (
              <pre className="result-box">{formatJson(challenge)}</pre>
            ) : (
              <p className="hint">No challenge requested yet.</p>
            )}
          </section>
        </section>

        <section className="panel">
          <div className="panel-header">
            <h2>Accounts</h2>
            <span className={`badge ${accounts.length ? 'success' : 'neutral'}`}>
              {accounts.length} loaded
            </span>
          </div>

          <label>
            <span>Account ID</span>
            <input
              value={accountId}
              onChange={(event) => setAccountId(event.target.value)}
              placeholder="0x..."
            />
          </label>
          <div className="actions">
            <button onClick={() => void fetchAccount()}>Fetch account</button>
            <button onClick={() => void listAccountDeltas()}>List account deltas</button>
            <button onClick={() => void listAccountProposals()}>List account proposals</button>
          </div>

          {accounts.length ? (
            <div className="account-list">
              {accounts.map((entry) => (
                <article className="account-card" key={entry.accountId}>
                  <div className="account-card-header">
                    <code>{entry.accountId}</code>
                    <span
                      className={`badge ${entry.stateStatus === 'available' ? 'success' : 'warning'}`}
                    >
                      {entry.stateStatus}
                    </span>
                  </div>
                  <div className="status-grid compact">
                    <div>
                      <span className="label">Scheme</span>
                      <strong>{entry.authScheme}</strong>
                    </div>
                    <div>
                      <span className="label">Authorized signers</span>
                      <strong>{entry.authorizedSignerCount}</strong>
                    </div>
                    <div>
                      <span className="label">Pending candidate</span>
                      <strong>{entry.hasPendingCandidate ? 'yes' : 'no'}</strong>
                    </div>
                    <div>
                      <span className="label">Updated at</span>
                      <strong>{entry.updatedAt}</strong>
                    </div>
                  </div>
                </article>
              ))}
            </div>
          ) : (
            <p className="hint">No account list loaded yet.</p>
          )}
        </section>

        <section className="panel">
          <div className="panel-header">
            <h2>Paged Accounts (Load More)</h2>
            <span className={`badge ${pagedCursor ? 'warning' : pagedPageCount > 0 ? 'success' : 'neutral'}`}>
              {pagedPageCount === 0
                ? 'not started'
                : pagedCursor
                  ? `${pagedAccounts.length} loaded · more available`
                  : `${pagedAccounts.length} loaded · end`}
            </span>
          </div>

          <label>
            <span>Page size (limit)</span>
            <input
              value={pagedLimit}
              onChange={(event) => setPagedLimit(event.target.value)}
              placeholder="2"
            />
          </label>
          <div className="actions">
            <button onClick={() => void loadFirstPage()}>Load first page</button>
            <button onClick={() => void loadMore()} disabled={!pagedCursor}>
              Load more
            </button>
            <button onClick={() => resetPagination()}>Reset</button>
          </div>

          <p className="hint">
            Pages loaded: <code>{pagedPageCount}</code>; nextCursor:{' '}
            <code>{pagedCursor ?? 'null'}</code>
          </p>

          {pagedAccounts.length ? (
            <div className="account-list">
              {pagedAccounts.map((entry, index) => (
                <article className="account-card" key={`${entry.accountId}-${index}`}>
                  <div className="account-card-header">
                    <code>
                      #{index + 1} · {entry.accountId}
                    </code>
                    <span
                      className={`badge ${entry.stateStatus === 'available' ? 'success' : 'warning'}`}
                    >
                      {entry.stateStatus}
                    </span>
                  </div>
                  <div className="status-grid compact">
                    <div>
                      <span className="label">Scheme</span>
                      <strong>{entry.authScheme}</strong>
                    </div>
                    <div>
                      <span className="label">Authorized signers</span>
                      <strong>{entry.authorizedSignerCount}</strong>
                    </div>
                  </div>
                </article>
              ))}
            </div>
          ) : (
            <p className="hint">Click <strong>Load first page</strong> to start paginating.</p>
          )}
        </section>

        <section className="two-column">
          <section className="panel">
            <div className="panel-header">
              <h2>Account Detail</h2>
            </div>
            {account ? (
              <pre className="result-box">{formatJson(account)}</pre>
            ) : (
              <p className="hint">Fetch one account to inspect the detail payload.</p>
            )}
          </section>

          <section className="panel">
            <div className="panel-header">
              <h2>Last Result</h2>
            </div>
            {lastResult ? (
              <pre className="result-box">{lastResult}</pre>
            ) : (
              <p className="hint">Successful responses appear here.</p>
            )}
          </section>
        </section>
      </main>
    </div>
  );
}
