import { p256 } from '@noble/curves/nist.js';
import { RequestAuthPayload } from '@openzeppelin/guardian-client';
import type { GuardianHttpClient, Signer } from '@openzeppelin/guardian-client';
import type { MidenClient } from '@miden-sdk/miden-sdk';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { MultisigClient } from '../src/client.js';

import { describeSessionGrant, sessionLogoutDigest } from '../src/session/grant.js';
import {
  GuardianSession,
  IndexedDbSessionKeyStore,
  type SessionKeyStore,
  type StoredSessionKey,
  WebCryptoSessionKey,
} from '../src/session/key.js';
import {
  GuardianSessionsUnsupportedError,
  SessionGrantDeclinedError,
  endGuardianSession,
  resumeGuardianSession,
  revokeAllGuardianSessions,
  startGuardianSession,
} from '../src/session/manager.js';
import { AuthDigest } from '../src/utils/digest.js';
import { hexToBytes } from '../src/utils/encoding.js';
import { wordToBytes } from '../src/utils/word.js';

const ACCOUNT_ID = '0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b';
const SIGNER_COMMITMENT = '0x' + '11'.repeat(32);
const GUARDIAN_COMMITMENT = '0x' + '22'.repeat(32);

function verifies(publicKey: string, message: Uint8Array, signature: string): boolean {
  return p256.verify(hexToBytes(signature), message, hexToBytes(publicKey), {
    prehash: true,
    lowS: false,
  });
}

function fakeGuardian(maxTtlSeconds = 28800) {
  return {
    getStatus: vi.fn().mockResolvedValue({
      status: 'ok',
      environment: 'devnet',
      sessions: { maxTtlSeconds },
    }),
    getPubkey: vi.fn().mockResolvedValue({ commitment: GUARDIAN_COMMITMENT }),
    createSession: vi.fn().mockResolvedValue({ signerCommitment: SIGNER_COMMITMENT, expiresAt: '' }),
    setSession: vi.fn(),
    setSigner: vi.fn(),
    getSession: vi.fn().mockReturnValue(null),
    revokeSession: vi.fn().mockResolvedValue(true),
    revokeAllSessions: vi.fn().mockResolvedValue(2),
  };
}

function fakeSigner(overrides: Partial<Signer> = {}): Signer {
  return {
    commitment: SIGNER_COMMITMENT,
    publicKey: '0x03' + '33'.repeat(32),
    scheme: 'ecdsa',
    signAccountIdWithTimestamp: vi.fn(),
    signCommitment: vi.fn(),
    signSessionGrant: vi.fn().mockResolvedValue('0xgrant-signature'),
    signSessionRevokeAll: vi.fn().mockResolvedValue('0xrevoke-all-signature'),
    ...overrides,
  };
}

/** Raw wallets must show the grant before signing; tests approve it. */
const approve = { confirm: () => true };

function memoryStore(): SessionKeyStore & { records: Map<string, StoredSessionKey> } {
  const records = new Map<string, StoredSessionKey>();
  return {
    records,
    load: async (id) => records.get(id) ?? null,
    save: async (record) => {
      records.set(record.id, record);
    },
    remove: async (id) => {
      records.delete(id);
    },
  };
}

describe('WebCryptoSessionKey', () => {
  it('keeps the private key non-extractable', async () => {
    const key = await WebCryptoSessionKey.generate();

    expect(key.privateKey.extractable).toBe(false);
    await expect(crypto.subtle.exportKey('pkcs8', key.privateKey)).rejects.toThrow();
    expect(hexToBytes(key.publicKey)).toHaveLength(33);
    expect(['02', '03']).toContain(key.publicKey.slice(2, 4));
  });
});

describe('GuardianSession', () => {
  it('signs requests over the same digest a wallet signs', async () => {
    const key = await WebCryptoSessionKey.generate();
    const session = new GuardianSession(key, SIGNER_COMMITMENT, 2_000_000_000, 'store-id');
    const payload = RequestAuthPayload.fromRequest({ account_id: ACCOUNT_ID });

    const signature = await session.signRequest(ACCOUNT_ID, 1_791_280_800_000, payload);

    const digest = wordToBytes(AuthDigest.fromRequest(ACCOUNT_ID, 1_791_280_800_000, payload));
    expect(verifies(session.publicKey, digest, signature)).toBe(true);
    const otherDigest = wordToBytes(
      AuthDigest.fromRequest(ACCOUNT_ID, 1_791_280_800_001, payload),
    );
    expect(verifies(session.publicKey, otherDigest, signature)).toBe(false);
  });

  it('signs the account-less logout message', async () => {
    const key = await WebCryptoSessionKey.generate();
    const session = new GuardianSession(key, SIGNER_COMMITMENT, 2_000_000_000, 'store-id');

    const signature = await session.signLogout(1_791_280_800_000);

    const digest = wordToBytes(sessionLogoutDigest(session.publicKey, 1_791_280_800_000));
    expect(verifies(session.publicKey, digest, signature)).toBe(true);
  });
});

describe('startGuardianSession', () => {
  it('builds the grant from the Guardian status and registers it', async () => {
    const guardian = fakeGuardian();
    const signer = fakeSigner();
    const store = memoryStore();
    const before = Math.floor(Date.now() / 1000);

    const session = await startGuardianSession(
      guardian as unknown as GuardianHttpClient,
      signer,
      { ttlSeconds: 3600, store, ...approve },
    );

    const grant = vi.mocked(signer.signSessionGrant!).mock.calls[0][0];
    expect(grant).toMatchObject({
      signerCommitment: SIGNER_COMMITMENT,
      sessionPublicKey: session.publicKey,
      origin: '',
      guardianCommitment: GUARDIAN_COMMITMENT,
      network: 'devnet',
    });
    expect(grant).not.toHaveProperty('guardian');
    expect(grant.issuedAt).toBeGreaterThanOrEqual(before);
    expect(grant.expiresAt - grant.issuedAt).toBe(3600);
    expect(guardian.getPubkey).toHaveBeenCalledWith('ecdsa');
    expect(guardian.createSession).toHaveBeenCalledWith({
      scheme: 'ecdsa',
      publicKey: signer.publicKey,
      signature: '0xgrant-signature',
      grant,
    });
    expect(guardian.setSession).toHaveBeenCalledWith(session);
    expect(session.signerCommitment).toBe(SIGNER_COMMITMENT);
    expect(session.storeId).toBe(`${GUARDIAN_COMMITMENT}:${SIGNER_COMMITMENT}`);
    expect(store.records.get(session.storeId)).toMatchObject({
      publicKey: session.publicKey,
      expiresAt: grant.expiresAt,
    });
  });

  it('names the page origin and lets the caller review the grant first', async () => {
    const guardian = fakeGuardian();
    const signer = fakeSigner();
    vi.stubGlobal('location', { origin: 'https://app.example' });
    const confirm = vi.fn().mockReturnValue(true);
    try {
      await startGuardianSession(guardian as unknown as GuardianHttpClient, signer, { confirm });
    } finally {
      vi.unstubAllGlobals();
    }

    const grant = vi.mocked(signer.signSessionGrant!).mock.calls[0][0];
    expect(grant.origin).toBe('https://app.example');
    expect(confirm).toHaveBeenCalledWith(grant);
    expect(describeSessionGrant(grant)).toContainEqual({
      label: 'Website',
      value: 'https://app.example',
    });

    const declined = fakeSigner();
    await expect(
      startGuardianSession(fakeGuardian() as unknown as GuardianHttpClient, declined, {
        origin: 'https://app.example',
        confirm: () => false,
      }),
    ).rejects.toBeInstanceOf(SessionGrantDeclinedError);
    expect(declined.signSessionGrant).not.toHaveBeenCalled();
  });

  it('switches the client signer only once the session starts', async () => {
    const declinedGuardian = fakeGuardian();
    await expect(
      startGuardianSession(declinedGuardian as unknown as GuardianHttpClient, fakeSigner(), {
        confirm: () => false,
      }),
    ).rejects.toBeInstanceOf(SessionGrantDeclinedError);
    expect(declinedGuardian.setSigner).not.toHaveBeenCalled();

    const olderGuardian = fakeGuardian();
    olderGuardian.getStatus.mockResolvedValue({ status: 'ok', environment: 'devnet' });
    await expect(
      startGuardianSession(olderGuardian as unknown as GuardianHttpClient, fakeSigner(), approve),
    ).rejects.toBeInstanceOf(GuardianSessionsUnsupportedError);
    expect(olderGuardian.setSigner).not.toHaveBeenCalled();

    const guardian = fakeGuardian();
    const signer = fakeSigner();
    const session = await startGuardianSession(
      guardian as unknown as GuardianHttpClient,
      signer,
      approve,
    );
    expect(guardian.setSigner).toHaveBeenCalledWith(signer);
    expect(guardian.setSigner.mock.invocationCallOrder[0]).toBeLessThan(
      guardian.setSession.mock.invocationCallOrder[0],
    );
    expect(guardian.setSession).toHaveBeenCalledWith(session);
  });

  it('requires a confirmation step for wallets that sign a hash', async () => {
    const raw = fakeSigner();
    const guardian = fakeGuardian();
    await expect(
      startGuardianSession(guardian as unknown as GuardianHttpClient, raw),
    ).rejects.toThrow(/pass `confirm`/);
    expect(raw.signSessionGrant).not.toHaveBeenCalled();
    expect(guardian.createSession).not.toHaveBeenCalled();

    // EIP-712 wallets display every grant field themselves.
    const eip712 = fakeSigner({ requestAuthFormat: 'eip712' });
    await startGuardianSession(guardian as unknown as GuardianHttpClient, eip712);
    expect(eip712.signSessionGrant).toHaveBeenCalledOnce();
  });

  it('dates the grant after the user confirms it and keeps the confirmed expiry', async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    try {
      const signer = fakeSigner();
      let shown: { issuedAt: number; expiresAt: number } | undefined;
      await startGuardianSession(fakeGuardian() as unknown as GuardianHttpClient, signer, {
        ttlSeconds: 3600,
        confirm: (grant) => {
          shown = { issuedAt: grant.issuedAt, expiresAt: grant.expiresAt };
          vi.setSystemTime(Date.now() + 240_000);
          return true;
        },
      });

      const signed = vi.mocked(signer.signSessionGrant!).mock.calls[0][0];
      expect(signed.issuedAt).toBe(shown!.issuedAt + 240);
      expect(signed.expiresAt).toBe(shown!.expiresAt);
    } finally {
      vi.useRealTimers();
    }
  });

  it('fails early when the wallet signs after Guardian would accept the grant', async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    try {
      const guardian = fakeGuardian();
      const slow = fakeSigner({
        signSessionGrant: vi.fn(async () => {
          vi.setSystemTime(Date.now() + 301_000);
          return '0xgrant-signature';
        }),
      });
      await expect(
        startGuardianSession(guardian as unknown as GuardianHttpClient, slow, approve),
      ).rejects.toThrow(/more than 300 seconds/);
      expect(guardian.createSession).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it('rejects a lifetime Guardian never accepts', async () => {
    const signer = fakeSigner();
    await expect(
      startGuardianSession(fakeGuardian() as unknown as GuardianHttpClient, signer, {
        ttlSeconds: 300,
        ...approve,
      }),
    ).rejects.toThrow(/above 300/);
    expect(signer.signSessionGrant).not.toHaveBeenCalled();
  });

  it('stores the key before using the session, and logs out if storing fails', async () => {
    const store = memoryStore();
    const guardian = fakeGuardian();
    const save = vi.spyOn(store, 'save');
    await startGuardianSession(guardian as unknown as GuardianHttpClient, fakeSigner(), {
      store,
      ...approve,
    });
    expect(save.mock.invocationCallOrder[0]).toBeLessThan(
      guardian.setSession.mock.invocationCallOrder[0],
    );

    const blocked = fakeGuardian();
    const failing = memoryStore();
    failing.save = vi.fn().mockRejectedValue(new Error('IndexedDB is blocked'));
    await expect(
      startGuardianSession(blocked as unknown as GuardianHttpClient, fakeSigner(), {
        store: failing,
        ...approve,
      }),
    ).rejects.toThrow(/blocked/);
    expect(blocked.setSession).not.toHaveBeenCalled();
    expect(blocked.revokeSession).toHaveBeenCalledOnce();
  });

  it('caps the lifetime below the Guardian maximum', async () => {
    const guardian = fakeGuardian(600);
    const signer = fakeSigner();

    await startGuardianSession(guardian as unknown as GuardianHttpClient, signer, {
      ttlSeconds: 86_400,
      ...approve,
    });

    const grant = vi.mocked(signer.signSessionGrant!).mock.calls[0][0];
    expect(grant.expiresAt - grant.issuedAt).toBe(540);
  });

  it('marks EIP-712 grants and omits the key for Falcon', async () => {
    const eip712 = fakeSigner({ requestAuthFormat: 'eip712' });
    const guardian = fakeGuardian();
    await startGuardianSession(guardian as unknown as GuardianHttpClient, eip712);
    expect(guardian.createSession.mock.calls[0][0]).toMatchObject({ authFormat: 'eip712' });

    const falcon = fakeSigner({ scheme: 'falcon' });
    const falconGuardian = fakeGuardian();
    await startGuardianSession(falconGuardian as unknown as GuardianHttpClient, falcon, approve);
    const request = falconGuardian.createSession.mock.calls[0][0];
    expect(request.scheme).toBe('falcon');
    expect(request).not.toHaveProperty('publicKey');
    expect(request).not.toHaveProperty('authFormat');
  });

  it('refuses when the Guardian or the signer cannot do sessions', async () => {
    const disabled = fakeGuardian();
    disabled.getStatus.mockResolvedValue({ status: 'ok', environment: 'devnet' });
    await expect(
      startGuardianSession(disabled as unknown as GuardianHttpClient, fakeSigner(), approve),
    ).rejects.toBeInstanceOf(GuardianSessionsUnsupportedError);

    await expect(
      startGuardianSession(
        fakeGuardian() as unknown as GuardianHttpClient,
        fakeSigner({ signSessionGrant: undefined }),
      ),
    ).rejects.toThrow(/cannot sign Guardian session grants/);
  });
});

describe('resume and end', () => {
  it('resumes a stored session without the wallet and forgets it on end', async () => {
    const store = memoryStore();
    const started = await startGuardianSession(
      fakeGuardian() as unknown as GuardianHttpClient,
      fakeSigner(),
      { store, ...approve },
    );

    const signer = fakeSigner();
    const guardian = fakeGuardian();
    const resumed = await resumeGuardianSession(
      guardian as unknown as GuardianHttpClient,
      signer,
      store,
    );

    expect(resumed?.publicKey).toBe(started.publicKey);
    expect(resumed?.signerCommitment).toBe(SIGNER_COMMITMENT);
    expect(guardian.setSession).toHaveBeenCalledWith(resumed);
    expect(signer.signSessionGrant).not.toHaveBeenCalled();

    await expect(
      endGuardianSession(guardian as unknown as GuardianHttpClient, resumed!, store),
    ).resolves.toBe(true);
    expect(guardian.revokeSession).toHaveBeenCalledWith(resumed);
    expect(store.records.size).toBe(0);
  });

  it('forgets the stored key when the session ends, but not a newer key', async () => {
    const store = memoryStore();
    const onEnded = vi.fn();
    const first = await startGuardianSession(
      fakeGuardian() as unknown as GuardianHttpClient,
      fakeSigner(),
      { store, onEnded, ...approve },
    );

    first.onEnded('revoked');
    expect(onEnded).toHaveBeenCalledExactlyOnceWith('revoked', first);
    await vi.waitFor(() => expect(store.records.size).toBe(0));

    const second = await startGuardianSession(
      fakeGuardian() as unknown as GuardianHttpClient,
      fakeSigner(),
      { store, ...approve },
    );
    first.onEnded('expired');
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(store.records.get(second.storeId)?.publicKey).toBe(second.publicKey);
  });

  it('revokes every session of the signer with the wallet and forgets the key', async () => {
    const store = memoryStore();
    const signer = fakeSigner();
    const started = await startGuardianSession(
      fakeGuardian() as unknown as GuardianHttpClient,
      signer,
      { store, ...approve },
    );
    expect(store.records.has(started.storeId)).toBe(true);

    const guardian = fakeGuardian();
    await expect(
      revokeAllGuardianSessions(guardian as unknown as GuardianHttpClient, signer, store),
    ).resolves.toBe(2);

    expect(guardian.revokeAllSessions).toHaveBeenCalledOnce();
    expect(guardian.setSigner).toHaveBeenCalledWith(signer);
    expect(store.records.size).toBe(0);
  });

  it('keeps a key another tab stored while revoking every session', async () => {
    const store = memoryStore();
    const signer = fakeSigner();
    const started = await startGuardianSession(
      fakeGuardian() as unknown as GuardianHttpClient,
      signer,
      { store, ...approve },
    );
    const newer = await WebCryptoSessionKey.generate();
    const guardian = fakeGuardian();
    guardian.revokeAllSessions.mockImplementation(async () => {
      await store.save({
        id: started.storeId,
        privateKey: newer.privateKey,
        publicKey: newer.publicKey,
        expiresAt: started.expiresAt,
      });
      return 1;
    });

    await revokeAllGuardianSessions(guardian as unknown as GuardianHttpClient, signer, store);

    expect(store.records.get(started.storeId)?.publicKey).toBe(newer.publicKey);
  });

  it('drops a stored session that has expired or is about to', async () => {
    for (const secondsLeft of [-1, 10]) {
      const store = memoryStore();
      const key = await WebCryptoSessionKey.generate();
      const id = `${GUARDIAN_COMMITMENT}:${SIGNER_COMMITMENT}`;
      await store.save({
        id,
        privateKey: key.privateKey,
        publicKey: key.publicKey,
        expiresAt: Math.floor(Date.now() / 1000) + secondsLeft,
      });

      const guardian = fakeGuardian();
      await expect(
        resumeGuardianSession(guardian as unknown as GuardianHttpClient, fakeSigner(), store),
      ).resolves.toBeNull();
      expect(store.records.has(id)).toBe(false);
      expect(guardian.setSession).not.toHaveBeenCalled();
    }
  });
});

describe('IndexedDbSessionKeyStore', () => {
  it('round-trips a non-extractable key that can still sign', async () => {
    const store = new IndexedDbSessionKeyStore(`guardian-sessions-test-${Date.now()}`);
    const key = await WebCryptoSessionKey.generate();
    const record: StoredSessionKey = {
      id: 'guardian:signer',
      privateKey: key.privateKey,
      publicKey: key.publicKey,
      expiresAt: 2_000_000_000,
    };

    await store.save(record);
    const loaded = await store.load('guardian:signer');
    expect(loaded?.publicKey).toBe(key.publicKey);
    expect(loaded?.privateKey.extractable).toBe(false);

    const session = new GuardianSession(
      new WebCryptoSessionKey(loaded!.privateKey, loaded!.publicKey),
      SIGNER_COMMITMENT,
      loaded!.expiresAt,
      loaded!.id,
    );
    const signature = await session.signLogout(1);
    expect(
      verifies(key.publicKey, wordToBytes(sessionLogoutDigest(key.publicKey, 1)), signature),
    ).toBe(true);

    await store.remove('guardian:signer');
    await expect(store.load('guardian:signer')).resolves.toBeNull();
  });

  it('retries opening the database after a failure', async () => {
    const store = new IndexedDbSessionKeyStore(`guardian-sessions-retry-${Date.now()}`);
    vi.stubGlobal('indexedDB', undefined);
    try {
      await expect(store.load('id')).rejects.toThrow(/not available/);
    } finally {
      vi.unstubAllGlobals();
    }
    await expect(store.load('id')).resolves.toBeNull();
  });
});

describe('MultisigClient sessions', () => {
  /** A Guardian that accepts every session call and answers reads with `readError`. */
  function guardianServer(readError?: string) {
    const fetch = vi.fn(async (url: string, init: RequestInit = {}) => {
      const path = new URL(url).pathname;
      if (path === '/state' && readError) {
        return {
          ok: false,
          status: 401,
          statusText: 'Unauthorized',
          headers: new Headers(),
          text: async () =>
            JSON.stringify({ code: readError, message: readError, meta: { retryable: false } }),
        };
      }
      const bodies: Record<string, unknown> = {
        '/status': { status: 'ok', environment: 'devnet', sessions: { max_ttl_seconds: 28800 } },
        '/pubkey': { commitment: GUARDIAN_COMMITMENT },
        '/session': { signer_commitment: SIGNER_COMMITMENT, expires_at: '' },
        '/session/logout': { revoked: true },
      };
      return { ok: true, status: 200, headers: new Headers(), json: async () => bodies[path], init };
    });
    vi.stubGlobal('fetch', fetch);
    return fetch;
  }

  function logouts(fetch: ReturnType<typeof guardianServer>): string[] {
    return fetch.mock.calls
      .filter(([url]) => new URL(url).pathname === '/session/logout')
      .map(([, init]) => (init!.headers as Record<string, string>)['x-pubkey']);
  }

  function multisigClient(): MultisigClient {
    return new MultisigClient({} as MidenClient, {
      guardianEndpoint: 'http://guardian.test',
      midenRpcEndpoint: 'http://rpc.test',
    });
  }

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('logs out the session it replaces', async () => {
    const fetch = guardianServer();
    const client = multisigClient();
    const signer = fakeSigner();

    const onEnded = vi.fn();
    const first = await client.startSession(signer, { onEnded, ...approve });
    const second = await client.startSession(signer, approve);

    expect(logouts(fetch)).toEqual([first.publicKey]);
    expect(onEnded).toHaveBeenCalledExactlyOnceWith('logout', first);
    expect(client.guardianClient.getSession()).toBe(second);
    await expect(client.endSession()).resolves.toBe(true);
    expect(logouts(fetch)).toEqual([first.publicKey, second.publicKey]);
    await expect(client.endSession()).resolves.toBe(false);
  });

  it('forgets a session Guardian ended and tells the app', async () => {
    const fetch = guardianServer('session_revoked');
    const client = multisigClient();
    const store = memoryStore();
    const onEnded = vi.fn();
    const session = await client.startSession(fakeSigner(), { store, onEnded, ...approve });

    await expect(client.guardianClient.getState(ACCOUNT_ID)).rejects.toMatchObject({
      code: 'session_revoked',
    });

    expect(onEnded).toHaveBeenCalledExactlyOnceWith('revoked', session);
    await vi.waitFor(() => expect(store.records.size).toBe(0));
    // Nothing left to log out.
    await expect(client.endSession()).resolves.toBe(false);
    expect(logouts(fetch)).toEqual([]);
  });

  it('keeps the session when logout fails', async () => {
    const fetch = guardianServer();
    const client = multisigClient();
    const session = await client.startSession(fakeSigner(), approve);
    fetch.mockRejectedValueOnce(new TypeError('network down'));

    await expect(client.endSession()).rejects.toThrow();

    expect(client.guardianClient.getSession()).toBe(session);
  });
});
