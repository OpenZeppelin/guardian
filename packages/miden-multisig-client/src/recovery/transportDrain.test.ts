import { describe, it, expect, vi } from 'vitest';
import type { MidenClient } from '@miden-sdk/miden-sdk';
import { freshDeviceStore as freshDevice } from '../testing/fake-indexeddb-device.js';
import { drainPrivateNoteBacklog } from './transportDrain.js';

/**
 * Classification tests use hand-rolled stub clients — the primitive takes the
 * `MidenClient` as an argument, so no module mocking is needed. Behavioral
 * tests (further down) run against the real WASM mock client.
 */
function stubClient(options: {
  /** Note count before the drain (first `notes.list()` call). */
  before: number;
  /** Note count after the drain (subsequent `notes.list()` calls). */
  after: number;
  fetchPrivate: () => Promise<void>;
  syncNoteTransport?: () => Promise<void>;
  /** Stored covered-tags value returned by `settings.get` (default absent). */
  coveredSnapshot?: unknown;
  /** Tracked note tags returned by `tags.list()` (default none). */
  tags?: number[];
}): {
  client: MidenClient;
  fetchPrivate: ReturnType<typeof vi.fn>;
  list: ReturnType<typeof vi.fn>;
  getSetting: ReturnType<typeof vi.fn>;
  setSetting: ReturnType<typeof vi.fn>;
  removeSetting: ReturnType<typeof vi.fn>;
  syncNoteTransport: ReturnType<typeof vi.fn>;
} {
  let call = 0;
  const fetchPrivate = vi.fn(options.fetchPrivate);
  const getSetting = vi.fn(async () => options.coveredSnapshot ?? null);
  const setSetting = vi.fn(async () => {});
  const removeSetting = vi.fn(async () => {});
  const syncNoteTransport = vi.fn(options.syncNoteTransport ?? (async () => {}));
  const list = vi.fn(async () => {
    const length = call === 0 ? options.before : options.after;
    call += 1;
    return new Array(length).fill({});
  });
  const client = {
    notes: { list, fetchPrivate },
    settings: { get: getSetting, set: setSetting, remove: removeSetting },
    tags: { list: vi.fn(async () => options.tags ?? []) },
    syncNoteTransport,
  } as unknown as MidenClient;
  return { client, fetchPrivate, list, getSetting, setSetting, removeSetting, syncNoteTransport };
}

describe('drainPrivateNoteBacklog', () => {
  it('reports completed with the count of newly imported records', async () => {
    const { client, fetchPrivate, removeSetting, syncNoteTransport } = stubClient({
      before: 1,
      after: 3,
      fetchPrivate: async () => {},
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(fetchPrivate).toHaveBeenCalledWith();
    // The covered-tags marker must be cleared before the transport sync so
    // every tracked tag is re-drained from the start (miden-sdk 0.16 moved
    // full-drain semantics into covered-tag bookkeeping).
    expect(removeSetting).toHaveBeenCalledWith('note_transport_covered_tags');
    expect(removeSetting.mock.invocationCallOrder[0]).toBeLessThan(
      syncNoteTransport.mock.invocationCallOrder[0],
    );
    expect(report).toEqual({ status: 'completed', imported: 2, retryable: false });
  });

  it('classifies a failure thrown by the transport sync stage like a drain failure', async () => {
    const { client } = stubClient({
      before: 0,
      after: 2,
      fetchPrivate: async () => {},
      syncNoteTransport: async () => {
        throw new TypeError('Failed to fetch');
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(report.status).toBe('failed');
    expect(report.retryable).toBe(true);
    expect(report.imported).toBe(2);
  });

  it('reports a disabled transport as unavailable, not retryable, without throwing', async () => {
    const { client } = stubClient({
      before: 0,
      after: 0,
      fetchPrivate: async () => {
        throw new Error(
          'note transport is disabled; enable it in the client configuration to send or receive notes via P2P',
        );
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(report.status).toBe('unavailable');
    expect(report.imported).toBe(0);
    expect(report.retryable).toBe(false);
    expect(report.reason).toContain('note transport is disabled');
  });

  it('skips the post-drain store re-count when the transport is disabled', async () => {
    const { client, list } = stubClient({
      before: 0,
      after: 0,
      fetchPrivate: async () => {
        throw new Error('note transport is disabled');
      },
    });

    await drainPrivateNoteBacklog(client);

    // A disabled transport throws before fetching anything, so only the
    // initial count runs.
    expect(list).toHaveBeenCalledTimes(1);
  });

  it('escalates a connection lost mid-drain with partial progress to a retryable failure', async () => {
    const { client } = stubClient({
      before: 0,
      after: 3,
      fetchPrivate: async () => {
        throw new TypeError('Failed to fetch');
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    // `unavailable` promises "nothing was imported"; with partial progress
    // the honest class is an interrupted, retryable drain.
    expect(report.status).toBe('failed');
    expect(report.retryable).toBe(true);
    expect(report.imported).toBe(3);
  });

  it('runs one transport sync per 64 tracked tags so no backfill candidate is deferred', async () => {
    const { client, syncNoteTransport } = stubClient({
      before: 0,
      after: 65,
      fetchPrivate: async () => {},
      tags: new Array(65).fill(0).map((_, i) => i),
    });

    const report = await drainPrivateNoteBacklog(client);

    // Upstream backfills at most MAX_BACKFILL_TAGS_PER_SYNC = 64 uncovered
    // tags per sync; with 65 tracked tags a single call would silently skip
    // the 65th while reporting completed.
    expect(syncNoteTransport).toHaveBeenCalledTimes(2);
    expect(report.status).toBe('completed');
  });

  it('restores the covered-tags snapshot when the drain fails after clearing it', async () => {
    const snapshot = new Uint8Array([1, 2, 3]);
    const { client, setSetting } = stubClient({
      before: 0,
      after: 0,
      fetchPrivate: async () => {},
      syncNoteTransport: async () => {
        throw new Error('note transport network error: 503');
      },
      coveredSnapshot: snapshot,
    });

    const report = await drainPrivateNoteBacklog(client);

    // Leaving the cleared marker in place would make every subsequent
    // normal sync re-attempt (and fail) the same per-tag backfill.
    expect(setSetting).toHaveBeenCalledWith('note_transport_covered_tags', snapshot);
    expect(report.status).toBe('unavailable');
    expect(report.retryable).toBe(true);
  });

  it('does not write a covered-tags value that never existed', async () => {
    const { client, setSetting } = stubClient({
      before: 0,
      after: 0,
      fetchPrivate: async () => {},
      syncNoteTransport: async () => {
        throw new Error('note transport network error: 503');
      },
    });

    await drainPrivateNoteBacklog(client);

    expect(setSetting).not.toHaveBeenCalled();
  });

  it('classifies a mid-drain node RPC failure as failed, not unavailable', async () => {
    const { client } = stubClient({
      before: 0,
      after: 0,
      fetchPrivate: async () => {},
      syncNoteTransport: async () => {
        throw new Error('grpc request failed for sync_state: unavailable');
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    // The node failing mid-import interrupted the drain; the transport
    // itself was reachable, so "proceed without transport notes" would be
    // the wrong guidance (mirrors the Rust `ClientError::RpcError` arm).
    expect(report.status).toBe('failed');
    expect(report.retryable).toBe(true);
  });

  it('classifies a permanently misconfigured transport endpoint as not retryable', async () => {
    const { client } = stubClient({
      before: 0,
      after: 0,
      fetchPrivate: async () => {
        throw new Error('connection error: invalid uri: missing scheme');
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    // Mirrors the Rust cause-chain classifier: a client that can never
    // connect must not tell recovery flows to loop retrying it.
    expect(report.status).toBe('unavailable');
    expect(report.retryable).toBe(false);
  });

  it('keeps a dropped connection retryable', async () => {
    const { client } = stubClient({
      before: 0,
      after: 0,
      fetchPrivate: async () => {
        throw new Error('connection error: connection refused');
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(report.status).toBe('unavailable');
    expect(report.retryable).toBe(true);
  });

  it('reports an unreachable transport as unavailable and retryable', async () => {
    const { client } = stubClient({
      before: 0,
      after: 0,
      fetchPrivate: async () => {
        throw new TypeError('Failed to fetch');
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(report.status).toBe('unavailable');
    expect(report.retryable).toBe(true);
  });

  it('reports the pagination convergence guard as a retryable failure, keeping the partial count', async () => {
    const { client } = stubClient({
      before: 0,
      after: 5,
      fetchPrivate: async () => {
        throw new Error(
          'fetch_all_private_notes did not converge after 1000 iterations — the server cursor is advancing but never returns an empty batch',
        );
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(report.status).toBe('failed');
    expect(report.retryable).toBe(true);
    // Batches imported before the failure stay imported and are counted.
    expect(report.imported).toBe(5);
  });

  it('reports unrecognized errors as a permanent failure', async () => {
    const { client } = stubClient({
      before: 0,
      after: 0,
      fetchPrivate: async () => {
        throw new Error('deserialization error: invalid note details');
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(report.status).toBe('failed');
    expect(report.retryable).toBe(false);
    expect(report.reason).toContain('deserialization');
  });

  it('classifies non-Error throws without crashing', async () => {
    const { client } = stubClient({
      before: 0,
      after: 0,
      fetchPrivate: async () => {
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        throw 'note transport is disabled';
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(report.status).toBe('unavailable');
    expect(report.reason).toBe('note transport is disabled');
  });

  it('propagates local store failures instead of misreporting them as transport outcomes', async () => {
    const client = {
      notes: {
        list: vi.fn(async () => {
          throw new Error('store is corrupted');
        }),
        fetchPrivate: vi.fn(),
      },
    } as unknown as MidenClient;

    await expect(drainPrivateNoteBacklog(client)).rejects.toThrow('store is corrupted');
  });

  it('rethrows a WASM storage error raised inside the drain', async () => {
    const { client } = stubClient({
      before: 0,
      after: 0,
      fetchPrivate: async () => {
        throw new Error('failed fetching all private notes: storage error: Indexdb error');
      },
    });

    await expect(drainPrivateNoteBacklog(client)).rejects.toThrow('storage error');
  });

  it('rethrows an IndexedDB transaction abort raised inside the drain', async () => {
    const abort = new Error('Transaction aborted');
    abort.name = 'AbortError';
    const { client } = stubClient({
      before: 0,
      after: 0,
      fetchPrivate: async () => {
        throw abort;
      },
    });

    await expect(drainPrivateNoteBacklog(client)).rejects.toThrow('Transaction aborted');
  });

  it('keeps a generic request abort as a transport report, not a store failure', async () => {
    const abort = new Error('The operation was aborted');
    abort.name = 'AbortError';
    const { client } = stubClient({
      before: 0,
      after: 0,
      fetchPrivate: async () => {
        throw abort;
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(report.status).toBe('unavailable');
    expect(report.retryable).toBe(true);
  });

  it('classifies throws with a non-string message without crashing', async () => {
    const { client } = stubClient({
      before: 0,
      after: 0,
      fetchPrivate: async () => {
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        throw { message: 42 };
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(report.status).toBe('failed');
    expect(report.retryable).toBe(false);
    expect(report.reason).toBe('42');
  });
});

/**
 * Raw access to the WASM store's note-transport cursor. The public
 * `settings` API runs values through the JS<->WASM serde codec, which is NOT
 * the store's raw encoding (seeding through it corrupts the cursor), so the
 * cursor tests read/write the IndexedDB row directly. Schema coupling, kept
 * minimal: store `settings`, keyPath `key`, `value` holds the raw big-endian
 * u64 bytes — the same representation the Rust cursor test pins.
 */
const CURSOR_KEY = 'note_transport_cursor';

async function withSettingsStore<T>(
  mode: IDBTransactionMode,
  operation: (store: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  const db: IDBDatabase = await new Promise((resolve, reject) => {
    const request = indexedDB.open('mock_client_db');
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  try {
    return await new Promise((resolve, reject) => {
      const request = operation(db.transaction('settings', mode).objectStore('settings'));
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
  } finally {
    db.close();
  }
}

async function seedRawCursor(bytes: Uint8Array): Promise<void> {
  await withSettingsStore('readwrite', (store) => store.put({ key: CURSOR_KEY, value: bytes }));
}

async function readRawCursor(): Promise<Uint8Array | undefined> {
  const row = await withSettingsStore<{ value?: Uint8Array } | undefined>('readonly', (store) =>
    store.get(CURSOR_KEY),
  );
  return row?.value;
}

/**
 * Behavioral tests against the real WASM mock client: the full device-loss
 * round trip the spike (#412) validated, entirely offline.
 */
describe('drainPrivateNoteBacklog (wasm mock client)', () => {
  it('recovers a transport-delivered private note into a fresh store, idempotently and tag-scoped', async () => {
    const { MidenClient, Account, createP2IDNote, NoteVisibility } = await import(
      '@miden-sdk/miden-sdk'
    );

    // Device A: create the account and relay a private self-addressed note
    // via the (mock) transport.
    freshDevice();
    const deviceA = await MidenClient.createMock();
    const account = await deviceA.accounts.create();
    const accountBytes = account.serialize();
    // miden-sdk 0.16 requires at least one asset on a P2ID note. The token
    // is a syntactically valid fungible-faucet account id (the same constant
    // the Rust test fixtures use); nothing is minted from it — the note only
    // travels the mock transport.
    const note = await createP2IDNote({
      from: account,
      to: account,
      assets: { token: '0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c', amount: 100 },
      type: NoteVisibility.Private,
    });
    // The mock chain never commits the note, so any at-or-below-commitment
    // hint works; 0 is always valid.
    await deviceA.notes.sendPrivate({ note, to: account, scanAfterBlockNum: 0 });
    const transportState = await deviceA.serializeMockNoteTransportNode();

    // Device B ("new device after loss"): fresh store sharing the same
    // transport backlog. Inserting the recovered account tracks its note tag
    // (the store invariant the drain depends on).
    freshDevice();
    const deviceB = await MidenClient.createMock({ serializedNoteTransport: transportState });
    expect(await deviceB.notes.list()).toHaveLength(0);
    await deviceB.accounts.insert({ account: Account.deserialize(accountBytes), overwrite: true });

    // Simulate a store whose cursor another account's sync already advanced
    // far past this backlog.
    const advancedCursor = new Uint8Array(8).fill(0xff);
    await seedRawCursor(advancedCursor);

    const first = await drainPrivateNoteBacklog(deviceB);
    expect(first.status).toBe('completed');
    expect(first.imported).toBe(1);
    expect(first.retryable).toBe(false);

    // The drain ignored the advanced cursor for scanning (the note above was
    // still recovered) and did not regress it afterward.
    expect(await readRawCursor()).toEqual(advancedCursor);

    // Idempotence: draining again re-fetches the same backlog but imports
    // nothing new.
    const second = await drainPrivateNoteBacklog(deviceB);
    expect(second.status).toBe('completed');
    expect(second.imported).toBe(0);

    // Cursor sanity: the incremental fetch after a drain is a no-op.
    await deviceB.notes.fetchPrivate();
    expect(await deviceB.notes.list()).toHaveLength(1);

    // Tag-scoping: a fresh store that does NOT track the account's tag
    // drains nothing from the same backlog.
    freshDevice();
    const blindDevice = await MidenClient.createMock({ serializedNoteTransport: transportState });
    const blind = await drainPrivateNoteBacklog(blindDevice);
    expect(blind.status).toBe('completed');
    expect(blind.imported).toBe(0);
  }, 120_000);

  it('tracks the standard note tag when a recovered account is inserted', async () => {
    const { MidenClient, Account, NoteTag, AccountId } = await import('@miden-sdk/miden-sdk');

    freshDevice();
    const deviceA = await MidenClient.createMock();
    const account = await deviceA.accounts.create();
    const accountBytes = account.serialize();
    const accountIdHex = account.id().toString();

    freshDevice();
    const deviceB = await MidenClient.createMock();
    expect(await deviceB.tags.list()).toHaveLength(0);

    await deviceB.accounts.insert({ account: Account.deserialize(accountBytes), overwrite: true });

    const expectedTag = NoteTag.withAccountTarget(AccountId.fromHex(accountIdHex)).asU32();
    expect(await deviceB.tags.list()).toContain(expectedTag);

    // Reload path: inserting the same account again must stay idempotent.
    await deviceB.accounts.insert({ account: Account.deserialize(accountBytes), overwrite: true });
    const tags = (await deviceB.tags.list()).filter((tag) => tag === expectedTag);
    expect(tags).toHaveLength(1);
  }, 120_000);
});
