import type {
  GuardianHttpClient,
  SessionGrantFields,
  Signer,
} from '@openzeppelin/guardian-client';
import {
  GuardianSession,
  type GuardianSessionOptions,
  type SessionKeyStore,
  type StoredSessionKey,
  WebCryptoSessionKey,
  forgetSessionKey,
} from './key.js';

/**
 * A grant is shortened by this much below the Guardian's maximum lifetime so
 * client clock drift cannot push it over the limit.
 */
const MAX_TTL_MARGIN_SECONDS = 60;

/**
 * Guardian accepts a grant only while it is at most this old, and only if it
 * expires more than this far in the future.
 */
const GRANT_WINDOW_SECONDS = 300;

/** Matches `GuardianHttpClient`: a session this close to expiry is not used. */
const SESSION_EXPIRY_MARGIN_SECONDS = 30;

/** The Guardian does not advertise sessions on `GET /status`. */
export class GuardianSessionsUnsupportedError extends Error {
  constructor() {
    super('This Guardian does not accept sessions');
    this.name = 'GuardianSessionsUnsupportedError';
  }
}

/** `confirm` returned `false`: the user declined the grant. */
export class SessionGrantDeclinedError extends Error {
  constructor() {
    super('Session grant was declined');
    this.name = 'SessionGrantDeclinedError';
  }
}

export interface StartSessionOptions extends Pick<GuardianSessionOptions, 'onEnded'> {
  /**
   * Requested lifetime, more than 300 seconds; capped below the Guardian's
   * advertised maximum.
   */
  ttlSeconds?: number;
  /**
   * The website asking for the session, shown to the user by the wallet.
   * Defaults to the page's `location.origin` in a browser and to none
   * elsewhere. Guardian does not check it against requests.
   */
  origin?: string;
  /**
   * Called with the grant before the wallet signs it; return `false` to
   * abort. Required for raw (non-EIP-712) wallets, which show only a hash:
   * display {@link describeSessionGrant} to the user here.
   */
  confirm?: (grant: SessionGrantFields) => boolean | Promise<boolean>;
  /** Persist the session key so a reload can `resume` without the wallet. */
  store?: SessionKeyStore;
}

/** The current page's origin in a browser; empty elsewhere. */
function pageOrigin(): string {
  const location = (globalThis as { location?: { origin?: string } }).location;
  return location?.origin && location.origin !== 'null' ? location.origin : '';
}

function storeId(guardianCommitment: string, signerCommitment: string): string {
  return `${guardianCommitment.toLowerCase()}:${signerCommitment.toLowerCase()}`;
}

function nowSeconds(): number {
  return Math.floor(Date.now() / 1000);
}

/**
 * Start a Guardian session: `signer` signs one grant and, once the session is
 * registered, becomes `guardian`'s signer; a non-extractable P-256 delegated
 * signer then signs the
 * session-eligible requests on `guardian` (reads, proposal list/get, proposal
 * create/sign). Every other route, including `configure`, `pushDelta`,
 * `abandonCandidate`, account lookup and `revokeAllSessions`, keeps using the
 * wallet. A session already set on `guardian` is replaced but stays valid on
 * the server until it expires; end it first with {@link endGuardianSession}.
 *
 * The grant is dated after `confirm` returns, so a slow confirmation still
 * registers; only its start moves, the confirmed expiry stays. If storing the
 * key fails, the registered session is logged out and the error rethrown.
 */
export async function startGuardianSession(
  guardian: GuardianHttpClient,
  signer: Signer,
  options: StartSessionOptions = {},
): Promise<GuardianSession> {
  if (!signer.signSessionGrant) {
    throw new Error('This signer cannot sign Guardian session grants');
  }
  if (signer.requestAuthFormat !== 'eip712' && !options.confirm) {
    throw new Error(
      'This wallet signs a session grant as a hash: pass `confirm` and show ' +
        'describeSessionGrant(grant) to the user first',
    );
  }
  const [status, { commitment: guardianCommitment }, key] = await Promise.all([
    guardian.getStatus(),
    guardian.getPubkey(signer.scheme),
    WebCryptoSessionKey.generate(),
  ]);
  if (!status.sessions) {
    throw new GuardianSessionsUnsupportedError();
  }
  const maxTtl = status.sessions.maxTtlSeconds - MAX_TTL_MARGIN_SECONDS;
  const ttl = Math.min(options.ttlSeconds ?? maxTtl, maxTtl);
  if (!Number.isInteger(ttl) || ttl <= GRANT_WINDOW_SECONDS) {
    throw new Error(
      `Session lifetime must be a whole number of seconds above ${GRANT_WINDOW_SECONDS}`,
    );
  }

  const issuedAt = nowSeconds();
  let grant: SessionGrantFields = {
    signerCommitment: signer.commitment,
    sessionPublicKey: key.publicKey,
    origin: options.origin ?? pageOrigin(),
    issuedAt,
    expiresAt: issuedAt + ttl,
    guardianCommitment,
    network: status.environment,
  };
  if (options.confirm) {
    if (!(await options.confirm(grant))) {
      throw new SessionGrantDeclinedError();
    }
    grant = { ...grant, issuedAt: Math.max(issuedAt, nowSeconds()) };
    if (grant.expiresAt - grant.issuedAt <= GRANT_WINDOW_SECONDS) {
      throw new Error('The session grant was confirmed too late; start the session again');
    }
  }
  const signature = await signer.signSessionGrant(grant);
  if (nowSeconds() - grant.issuedAt > GRANT_WINDOW_SECONDS) {
    throw new Error(
      `The wallet took more than ${GRANT_WINDOW_SECONDS} seconds to sign the session grant; ` +
        'start the session again',
    );
  }
  await guardian.createSession({
    scheme: signer.scheme,
    ...(signer.requestAuthFormat === 'eip712' ? { authFormat: 'eip712' as const } : {}),
    ...(signer.scheme === 'ecdsa' ? { publicKey: signer.publicKey } : {}),
    signature,
    grant,
  });

  const session = new GuardianSession(
    key,
    signer.commitment,
    grant.expiresAt,
    storeId(guardianCommitment, signer.commitment),
    { store: options.store, onEnded: options.onEnded },
  );
  if (options.store) {
    try {
      await options.store.save({
        id: session.storeId,
        privateKey: key.privateKey,
        publicKey: key.publicKey,
        expiresAt: grant.expiresAt,
      });
    } catch (error) {
      const unreachable = new GuardianSession(key, signer.commitment, grant.expiresAt, session.storeId);
      await guardian.revokeSession(unreachable).catch(() => false);
      throw error;
    }
  }
  guardian.setSigner(signer);
  guardian.setSession(session);
  return session;
}

/**
 * Reuse a session key persisted by `startGuardianSession` without asking the
 * wallet again; `signer` becomes `guardian`'s signer when a session resumes.
 * Returns `null` when none
 * is stored or it expires within 30 seconds. A session revoked elsewhere
 * fails its next request with `session_revoked` and ends then (`onEnded`);
 * start a new one.
 */
export async function resumeGuardianSession(
  guardian: GuardianHttpClient,
  signer: Signer,
  store: SessionKeyStore,
  options: Pick<GuardianSessionOptions, 'onEnded'> = {},
): Promise<GuardianSession | null> {
  const { commitment: guardianCommitment } = await guardian.getPubkey(signer.scheme);
  const id = storeId(guardianCommitment, signer.commitment);
  const record = await store.load(id);
  if (!record) {
    return null;
  }
  if (record.expiresAt - SESSION_EXPIRY_MARGIN_SECONDS <= nowSeconds()) {
    await store.remove(id);
    return null;
  }
  const session = new GuardianSession(
    new WebCryptoSessionKey(record.privateKey, record.publicKey),
    signer.commitment,
    record.expiresAt,
    id,
    { store, onEnded: options.onEnded },
  );
  guardian.setSigner(signer);
  guardian.setSession(session);
  return session;
}

/**
 * Revoke `session` on the server, stop using it if it is the current one, and
 * forget its stored key. Returns whether the server still had it active. On
 * error nothing changes.
 */
export async function endGuardianSession(
  guardian: GuardianHttpClient,
  session: GuardianSession,
  store?: SessionKeyStore,
): Promise<boolean> {
  const revoked = await guardian.revokeSession(session);
  if (store) {
    await forgetSessionKey(store, session);
  }
  return revoked;
}

/**
 * Revoke every session of `signer` on the Guardian with a wallet signature,
 * including sessions started in other tabs or devices, stop using the current
 * one, and forget its stored key (a newer key another tab stored meanwhile
 * is kept). `signer` becomes `guardian`'s signer. Use it when a session key
 * may be compromised: page logout alone cannot stop a script that holds the
 * key. Returns how many sessions the server revoked.
 *
 * It ends sessions already registered with Guardian. A page that got a grant
 * signed can still register it for up to about 10 minutes, and a device whose
 * clock runs behind can miss sessions started just before; run it again 10
 * minutes later when a key may be compromised.
 */
export async function revokeAllGuardianSessions(
  guardian: GuardianHttpClient,
  signer: Signer,
  store?: SessionKeyStore,
): Promise<number> {
  guardian.setSigner(signer);
  const stored = store ? await storedKey(guardian, signer, store) : null;
  const revoked = await guardian.revokeAllSessions();
  if (store && stored) {
    await forgetSessionKey(store, { storeId: stored.id, publicKey: stored.publicKey });
  }
  return revoked;
}

async function storedKey(
  guardian: GuardianHttpClient,
  signer: Signer,
  store: SessionKeyStore,
): Promise<StoredSessionKey | null> {
  const { commitment: guardianCommitment } = await guardian.getPubkey(signer.scheme);
  return store.load(storeId(guardianCommitment, signer.commitment));
}
