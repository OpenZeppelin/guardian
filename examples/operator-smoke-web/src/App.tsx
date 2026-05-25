import { useEffect, useMemo, useState } from 'react';
import {
  GuardianOperatorHttpClient,
  GuardianOperatorHttpError,
  type DashboardAccountDetail,
  type DashboardAccountSummary,
  type DashboardDeltaCategory,
  type DashboardDeltaEntry,
  type DashboardGlobalDeltaEntry,
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
    const data = error.data;
    const base = data?.error ?? error.message;
    // Surface the normalized code + paused-specific details so the
    // smoke makes the wire→client mapping visible (e.g. server emits
    // `GUARDIAN_ACCOUNT_PAUSED`, client surfaces `account_paused`
    // with `pausedAt` / `pausedReason`).
    if (data?.code === 'account_paused') {
      return `[${data.code}] ${base} (pausedAt=${data.pausedAt ?? 'null'}, pausedReason=${data.pausedReason ?? 'null'})`;
    }
    if (data?.code) {
      return `[${data.code}] ${base}`;
    }
    return base;
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

const DELTA_CATEGORY_LABEL: Record<DashboardDeltaCategory, string> = {
  asset_transfer: 'Asset transfer',
  note_consumption: 'Notes consumed',
  note_creation: 'Notes created',
  account_storage_change: 'Account / storage change',
  guardian_switch: 'Guardian switch',
  custom: 'Custom / unknown',
};

const DELTA_CATEGORY_BADGE: Record<DashboardDeltaCategory, string> = {
  asset_transfer: 'success',
  note_consumption: 'neutral',
  note_creation: 'neutral',
  account_storage_change: 'warning',
  guardian_switch: 'warning',
  custom: 'neutral',
};

function isGlobalDeltaEntry(
  entry: DashboardDeltaEntry | DashboardGlobalDeltaEntry,
): entry is DashboardGlobalDeltaEntry {
  return typeof entry.accountId === 'string' && entry.accountId.length > 0;
}

/**
 * Build a short human-readable one-line summary from an enriched
 * delta entry. This is the rule-of-thumb test from spec.md §SC-001 —
 * "an operator (or downstream renderer) can produce a one-line human
 * summary using only the returned fields."
 *
 * Reads from `metadata.category`, `metadata.proposal.proposal_type`,
 * `metadata.asset`, `metadata.counterparty`, and `metadata.note_counts`.
 * Returns a placeholder when the entry has no `metadata` block
 * (pre-feature-007 historical rows).
 */
function describeDelta(
  entry: DashboardDeltaEntry | DashboardGlobalDeltaEntry,
): string {
  const meta = entry.metadata;
  if (!meta) {
    // Three cases produce absent metadata: EVM deltas, pre-feature-007
    // historical rows whose proposal was already deleted, and rows
    // whose `TransactionSummary` is undecodable. The dashboard does
    // not fabricate `category: custom` / `note_counts: {0,0}` in
    // these cases because the source data may actually represent
    // real activity (e.g. a historical consume_notes whose details
    // we just don't have indexed).
    return 'metadata unavailable';
  }
  const kind = meta.proposal?.proposalType;
  switch (meta.category) {
    case 'asset_transfer': {
      const recipient = meta.counterparty?.accountId ?? 'unknown recipient';
      const amount = meta.asset?.amount ?? '?';
      const asset = meta.asset?.assetId ?? 'asset';
      return `Transferred ${amount} of ${asset} → ${recipient}`;
    }
    case 'note_consumption':
      return `Consumed ${meta.noteCounts.input} note${meta.noteCounts.input === 1 ? '' : 's'}`;
    case 'note_creation':
      return `Created ${meta.noteCounts.output} note${meta.noteCounts.output === 1 ? '' : 's'}`;
    case 'account_storage_change':
      return kind ? `Account change: ${kind}` : 'Account / storage change';
    case 'guardian_switch':
      return 'Switched Guardian';
    case 'custom':
      return kind ? `Custom (${kind})` : 'Custom / unknown';
  }
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
  const [pauseReason, setPauseReason] = useState('smoke-test');
  const [pagedLimit, setPagedLimit] = useState('2');
  const [pagedAccounts, setPagedAccounts] = useState<DashboardAccountSummary[]>([]);
  const [pagedCursor, setPagedCursor] = useState<string | null>(null);
  const [pagedPageCount, setPagedPageCount] = useState(0);
  const [globalDeltaStatusFilter, setGlobalDeltaStatusFilter] = useState<{
    candidate: boolean;
    canonical: boolean;
    discarded: boolean;
  }>({ candidate: false, canonical: false, discarded: false });

  // Feature 007: structured delta-list rendering for the new
  // category / kind / summary wire fields. We keep the raw JSON dump
  // (`lastResult`) too, but the panel below surfaces the human-meaningful
  // fields directly so the harness proves end-to-end that the enrichment
  // flows from server → TS client → operator UI.
  const [deltaList, setDeltaList] = useState<
    (DashboardDeltaEntry | DashboardGlobalDeltaEntry)[]
  >([]);
  const [deltaListSource, setDeltaListSource] = useState<
    'account' | 'global' | null
  >(null);
  const [deltaListLabel, setDeltaListLabel] = useState<string>('');

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

  async function listPausedAccounts() {
    await runAction('listAccounts(paused=true)', async () => {
      const response = await client.listAccounts({ paused: true });
      setAccounts(response.items);
      return response;
    });
  }

  async function listActiveAccounts() {
    await runAction('listAccounts(paused=false)', async () => {
      const response = await client.listAccounts({ paused: false });
      setAccounts(response.items);
      return response;
    });
  }

  async function pauseAccountAction() {
    await runAction('pauseAccount', () => {
      const id = accountId.trim();
      if (!id) throw new Error('Account ID is required');
      const reason = pauseReason.trim();
      if (!reason) throw new Error('Pause reason is required');
      return client.pauseAccount(id, reason);
    });
  }

  async function unpauseAccountAction() {
    await runAction('unpauseAccount', () => {
      const id = accountId.trim();
      if (!id) throw new Error('Account ID is required');
      const reason = pauseReason.trim();
      return client.unpauseAccount(id, reason || undefined);
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

  async function getSession() {
    await runAction('getSession', () => client.getSession());
  }

  async function listAccountDeltas() {
    await runAction('listAccountDeltas', async () => {
      const id = accountId.trim();
      if (!id) throw new Error('Account ID is required');
      const page = await client.listAccountDeltas(id);
      setDeltaList(page.items);
      setDeltaListSource('account');
      setDeltaListLabel(`per-account · ${id}`);
      return page;
    });
  }

  async function listAccountProposals() {
    await runAction('listAccountProposals', () => {
      const id = accountId.trim();
      if (!id) throw new Error('Account ID is required');
      return client.listAccountProposals(id);
    });
  }

  async function fetchAccountSnapshot() {
    await runAction('fetchAccountSnapshot', () => {
      const id = accountId.trim();
      if (!id) throw new Error('Account ID is required');
      return client.getAccountSnapshot(id);
    });
  }

  async function listGlobalDeltas() {
    await runAction('listGlobalDeltas', async () => {
      const selected = (
        ['candidate', 'canonical', 'discarded'] as const
      ).filter((s) => globalDeltaStatusFilter[s]);
      const page = await client.listGlobalDeltas(
        selected.length > 0 ? { status: selected } : {},
      );
      setDeltaList(page.items);
      setDeltaListSource('global');
      setDeltaListLabel(
        selected.length > 0
          ? `global · status=${selected.join(',')}`
          : 'global · (no filter)',
      );
      return page;
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
            <button onClick={() => void listPausedAccounts()}>List paused accounts</button>
            <button onClick={() => void listActiveAccounts()}>List active accounts</button>
            <button onClick={() => void paginateAccounts()}>Paginate accounts</button>
            <button onClick={() => void dashboardInfo()}>Dashboard info</button>
            <button onClick={() => void getSession()}>Get session</button>
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
            <button onClick={() => void fetchAccountSnapshot()}>Fetch snapshot</button>
            <button onClick={() => void listAccountDeltas()}>List account deltas</button>
            <button onClick={() => void listAccountProposals()}>List account proposals</button>
          </div>

          <label>
            <span>Pause reason</span>
            <input
              value={pauseReason}
              onChange={(event) => setPauseReason(event.target.value)}
              placeholder="why?"
            />
          </label>
          <div className="actions">
            <button onClick={() => void pauseAccountAction()}>Pause account</button>
            <button onClick={() => void unpauseAccountAction()}>Unpause account</button>
          </div>
          <p className="hint">
            Requires <code>accounts:pause</code>. After pausing, run{' '}
            <strong>List paused accounts</strong> or <strong>Get account</strong>{' '}
            against the same id — the response should expose{' '}
            <code>pausedAt</code> and <code>pausedReason</code>. The 409{' '}
            <code>account_paused</code> path is exercised by mutating endpoints
            (delta push, proposal create/sign) which this read-only harness does
            not call directly.
          </p>

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
            <h2>Delta feed (enriched)</h2>
            <span
              className={`badge ${
                deltaListSource === null
                  ? 'neutral'
                  : deltaList.length === 0
                    ? 'warning'
                    : 'success'
              }`}
            >
              {deltaListSource === null
                ? 'not loaded'
                : deltaList.length === 0
                  ? 'empty page'
                  : `${deltaList.length} entries · ${deltaListLabel}`}
            </span>
          </div>

          <p className="hint">
            Feature 007. Each entry exposes the new typed{' '}
            <code>metadata</code> blob: top-level <code>category</code>,{' '}
            <code>asset</code>, <code>counterparty</code>,{' '}
            <code>note_counts</code>, plus an optional <code>proposal</code>{' '}
            block carrying operator-stated intent for multisig commits. Use{' '}
            <strong>List account deltas</strong> or <strong>List global deltas</strong> to populate
            this panel. The one-line summary at the top of each card is derived
            purely from the fields the server returned — no detail endpoint
            required (SC-001).
          </p>

          {deltaList.length > 0 ? (
            <div className="delta-list">
              {deltaList.map((delta, index) => {
                const globalAccountId = isGlobalDeltaEntry(delta)
                  ? delta.accountId
                  : undefined;
                const cardKey = `${globalAccountId ?? 'acct'}-${delta.nonce}-${index}`;
                const meta = delta.metadata;
                const proposal = meta?.proposal;
                return (
                  <article className="delta-card" key={cardKey}>
                    <header className="delta-card-header">
                      <span className="delta-summary-line">
                        {describeDelta(delta)}
                      </span>
                      {meta ? (
                        <span
                          className={`badge ${DELTA_CATEGORY_BADGE[meta.category]}`}
                        >
                          {DELTA_CATEGORY_LABEL[meta.category]}
                        </span>
                      ) : (
                        <span className="badge neutral">metadata unavailable</span>
                      )}
                    </header>

                    <div className="status-grid compact">
                      <div>
                        <span className="label">Nonce</span>
                        <strong>{delta.nonce}</strong>
                      </div>
                      <div>
                        <span className="label">Status</span>
                        <strong>{delta.status}</strong>
                      </div>
                      <div>
                        <span className="label">Status timestamp</span>
                        <strong>{delta.statusTimestamp}</strong>
                      </div>
                      <div>
                        <span className="label">Proposal type</span>
                        <strong>
                          {proposal?.proposalType ?? (
                            <em className="muted">none</em>
                          )}
                        </strong>
                      </div>
                      {globalAccountId ? (
                        <div>
                          <span className="label">Account</span>
                          <strong>
                            <code>{globalAccountId}</code>
                          </strong>
                        </div>
                      ) : null}
                      <div>
                        <span className="label">Input / output notes</span>
                        <strong>
                          {meta
                            ? `${meta.noteCounts.input} / ${meta.noteCounts.output}`
                            : '—'}
                        </strong>
                      </div>
                    </div>

                    <div className="status-grid compact">
                      <div>
                        <span className="label">Asset</span>
                        <strong>
                          {meta?.asset ? (
                            <>
                              <code>{meta.asset.assetId}</code>
                              {meta.asset.amount ? (
                                <> · {meta.asset.amount}</>
                              ) : null}
                              <> · {meta.asset.kind}</>
                            </>
                          ) : (
                            <em className="muted">none</em>
                          )}
                        </strong>
                      </div>
                      <div>
                        <span className="label">Counterparty</span>
                        <strong>
                          {meta?.counterparty ? (
                            <>
                              <code>{meta.counterparty.accountId}</code>
                              <> · {meta.counterparty.direction}</>
                            </>
                          ) : (
                            <em className="muted">none</em>
                          )}
                        </strong>
                      </div>
                    </div>

                    {proposal ? (
                      <details className="delta-debug" open>
                        <summary>Proposal intent</summary>
                        <div className="status-grid compact">
                          <div>
                            <span className="label">proposal_type</span>
                            <code>{proposal.proposalType}</code>
                          </div>
                          {proposal.description ? (
                            <div>
                              <span className="label">description</span>
                              <strong>{proposal.description}</strong>
                            </div>
                          ) : null}
                          {proposal.recipientId ? (
                            <div>
                              <span className="label">recipient_id</span>
                              <code className="wrap">{proposal.recipientId}</code>
                            </div>
                          ) : null}
                          {proposal.faucetId ? (
                            <div>
                              <span className="label">faucet_id</span>
                              <code className="wrap">{proposal.faucetId}</code>
                            </div>
                          ) : null}
                          {proposal.amount ? (
                            <div>
                              <span className="label">amount</span>
                              <strong>{proposal.amount}</strong>
                            </div>
                          ) : null}
                          {typeof proposal.requiredSignatures === 'number' ? (
                            <div>
                              <span className="label">required_signatures</span>
                              <strong>{proposal.requiredSignatures}</strong>
                            </div>
                          ) : null}
                          {typeof proposal.targetThreshold === 'number' ? (
                            <div>
                              <span className="label">target_threshold</span>
                              <strong>{proposal.targetThreshold}</strong>
                            </div>
                          ) : null}
                          {proposal.noteIds && proposal.noteIds.length > 0 ? (
                            <div>
                              <span className="label">note_ids</span>
                              <code className="wrap">
                                {proposal.noteIds.join(', ')}
                              </code>
                            </div>
                          ) : null}
                          {proposal.newGuardianPubkey ? (
                            <div>
                              <span className="label">new_guardian_pubkey</span>
                              <code className="wrap">
                                {proposal.newGuardianPubkey}
                              </code>
                            </div>
                          ) : null}
                          {proposal.newGuardianEndpoint ? (
                            <div>
                              <span className="label">new_guardian_endpoint</span>
                              <code className="wrap">
                                {proposal.newGuardianEndpoint}
                              </code>
                            </div>
                          ) : null}
                        </div>
                      </details>
                    ) : null}

                    <details className="delta-debug">
                      <summary>Debug commitments</summary>
                      <div className="status-grid compact">
                        <div>
                          <span className="label">prev_commitment</span>
                          <code className="wrap">{delta.prevCommitment}</code>
                        </div>
                        <div>
                          <span className="label">new_commitment</span>
                          <code className="wrap">
                            {delta.newCommitment ?? 'null'}
                          </code>
                        </div>
                        {typeof delta.retryCount === 'number' ? (
                          <div>
                            <span className="label">retry_count</span>
                            <strong>{delta.retryCount}</strong>
                          </div>
                        ) : null}
                      </div>
                    </details>
                  </article>
                );
              })}
            </div>
          ) : (
            <p className="hint">No delta feed loaded yet.</p>
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
