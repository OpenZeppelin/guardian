import { describe, it, expect, vi } from 'vitest';
import type { MidenClient } from '@miden-sdk/miden-sdk';
import { freshDeviceStore as freshDevice } from '../testing/fake-indexeddb-device.js';
import { drainPrivateNoteBacklog, readTransportCursors } from './transportDrain.js';

let stubStores = 0;

/** Opens (creating on first use) a stub store laid out like the SDK's. */
async function openStubStore(name: string): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(name, 1);
    request.onupgradeneeded = () => {
      request.result.createObjectStore('settings', { keyPath: ['scope', 'key'] });
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

async function writeStubCursor(name: string, position: number): Promise<void> {
  const db = await openStubStore(name);
  try {
    await new Promise<void>((resolve, reject) => {
      const request = db
        .transaction('settings', 'readwrite')
        .objectStore('settings')
        .put({ scope: 0, key: 'note_transport_cursors', value: new Uint8Array([position]) });
      request.onsuccess = () => resolve();
      request.onerror = () => reject(request.error);
    });
  } finally {
    db.close();
  }
}

/**
 * Classification tests use hand-rolled stub clients — the primitive takes the
 * `MidenClient` as an argument, so no module mocking is needed. Each stub has
 * its own IndexedDB store with the SDK's `settings` layout, so the drain reads
 * real transport cursors. Behavioral tests (further down) run against the
 * real WASM mock client.
 */
async function stubClient(options: {
  /**
   * Note counts returned by successive `notes.list()` calls: the first is the
   * count before the drain. The last value repeats.
   */
  counts: number[];
  fetchPrivate: () => Promise<void>;
  /** How many successful fetches advance the transport cursor (default 0). */
  moves?: number;
}): Promise<{
  client: MidenClient;
  fetchPrivate: ReturnType<typeof vi.fn>;
  list: ReturnType<typeof vi.fn>;
}> {
  const name = `stub_store_${stubStores}`;
  stubStores += 1;
  (await openStubStore(name)).close();
  let call = 0;
  let fetches = 0;
  const fetchPrivate = vi.fn(async () => {
    await options.fetchPrivate();
    fetches += 1;
    if (fetches <= (options.moves ?? 0)) {
      await writeStubCursor(name, fetches);
    }
  });
  const list = vi.fn(async () => {
    const length = options.counts[Math.min(call, options.counts.length - 1)];
    call += 1;
    return new Array(length).fill({});
  });
  const client = {
    notes: { list, fetchPrivate },
    storeIdentifier: async () => name,
  } as unknown as MidenClient;
  return { client, fetchPrivate, list };
}

describe('drainPrivateNoteBacklog', () => {
  it('reports completed with the count of newly imported records', async () => {
    const { client, fetchPrivate } = await stubClient({
      counts: [1, 3],
      fetchPrivate: async () => {},
      moves: 1,
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(fetchPrivate).toHaveBeenCalledTimes(2);
    expect(report).toEqual({ status: 'completed', imported: 2, retryable: false });
  });

  it('keeps fetching until a fetch moves no transport cursor', async () => {
    const { client, fetchPrivate } = await stubClient({
      counts: [0, 5],
      fetchPrivate: async () => {},
      moves: 2,
    });

    const report = await drainPrivateNoteBacklog(client);

    // Each upstream fetch reads a bounded number of pages, so a long backlog
    // arrives over several fetches; stopping after the first would report
    // completed with notes still on the transport.
    expect(fetchPrivate).toHaveBeenCalledTimes(3);
    expect(report).toEqual({ status: 'completed', imported: 5, retryable: false });
  });

  it('keeps fetching while fetches move the cursor without importing anything', async () => {
    const { client, fetchPrivate } = await stubClient({
      counts: [0, 1],
      fetchPrivate: async () => {},
      moves: 3,
    });

    const report = await drainPrivateNoteBacklog(client);

    // A store upgraded from 0.18.0 restarts every tag from the first retained
    // note, so the first fetches walk history the store already holds and
    // import nothing; the note behind them is only reached by continuing.
    expect(fetchPrivate).toHaveBeenCalledTimes(4);
    expect(report).toEqual({ status: 'completed', imported: 1, retryable: false });
  });

  it('reports a backlog that outlasts the fetch budget as a retryable failure', async () => {
    const { client, fetchPrivate } = await stubClient({
      counts: [0, 64],
      fetchPrivate: async () => {},
      moves: Number.POSITIVE_INFINITY,
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(fetchPrivate).toHaveBeenCalledTimes(64);
    expect(report.status).toBe('failed');
    expect(report.retryable).toBe(true);
    expect(report.imported).toBe(64);
    expect(report.reason).toContain('after 64 fetches');
  });

  it('reports a disabled transport as unavailable, not retryable, without throwing', async () => {
    const { client } = await stubClient({
      counts: [0, 0],
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
    const { client, list } = await stubClient({
      counts: [0, 0],
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
    const { client } = await stubClient({
      counts: [0, 3],
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

  it('classifies a mid-drain node RPC failure as failed, not unavailable', async () => {
    const { client } = await stubClient({
      counts: [0, 0],
      fetchPrivate: async () => {
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
    const { client } = await stubClient({
      counts: [0, 0],
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
    const { client } = await stubClient({
      counts: [0, 0],
      fetchPrivate: async () => {
        throw new Error('connection error: connection refused');
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(report.status).toBe('unavailable');
    expect(report.retryable).toBe(true);
  });

  it('reports an unreachable transport as unavailable and retryable', async () => {
    const { client } = await stubClient({
      counts: [0, 0],
      fetchPrivate: async () => {
        throw new TypeError('Failed to fetch');
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(report.status).toBe('unavailable');
    expect(report.retryable).toBe(true);
  });

  it('reports the pagination convergence guard as a retryable failure, keeping the partial count', async () => {
    const { client } = await stubClient({
      counts: [0, 5],
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
    const { client } = await stubClient({
      counts: [0, 0],
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
    const { client } = await stubClient({
      counts: [0, 0],
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
    const { client } = await stubClient({
      counts: [0, 0],
      fetchPrivate: async () => {
        throw new Error('failed fetching all private notes: storage error: Indexdb error');
      },
    });

    await expect(drainPrivateNoteBacklog(client)).rejects.toThrow('storage error');
  });

  it('rethrows an IndexedDB transaction abort raised inside the drain', async () => {
    const abort = new Error('Transaction aborted');
    abort.name = 'AbortError';
    const { client } = await stubClient({
      counts: [0, 0],
      fetchPrivate: async () => {
        throw abort;
      },
    });

    await expect(drainPrivateNoteBacklog(client)).rejects.toThrow('Transaction aborted');
  });

  it('keeps a generic request abort as a transport report, not a store failure', async () => {
    const abort = new Error('The operation was aborted');
    abort.name = 'AbortError';
    const { client } = await stubClient({
      counts: [0, 0],
      fetchPrivate: async () => {
        throw abort;
      },
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(report.status).toBe('unavailable');
    expect(report.retryable).toBe(true);
  });

  it('classifies throws with a non-string message without crashing', async () => {
    const { client } = await stubClient({
      counts: [0, 0],
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
 * Behavioral tests against the real WASM mock client: the full device-loss
 * round trip the spike (#412) validated, entirely offline.
 */
describe('drainPrivateNoteBacklog (wasm mock client)', () => {
  it('recovers a transport-delivered private note into a fresh store, idempotently and tag-scoped', async () => {
    const { MidenClient, Account, createP2IDNote, NoteVisibility, NoteInclusionProof } =
      await import('@miden-sdk/miden-sdk');

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
    // The mock chain never commits the note and the mock transport does not
    // verify proofs, so a mock proof at genesis stands in for a real one.
    await deviceA.notes.sendPrivate({
      note,
      to: account,
      inclusionProof: NoteInclusionProof.mockAtBlock(0),
    });
    const transportState = await deviceA.serializeMockNoteTransportNode();
    // Clients sharing a store name share one IndexedDB connection while any
    // of them is alive, so release device A before simulating a new device.
    await deviceA[Symbol.asyncDispose]();

    // Device B ("new device after loss"): fresh store sharing the same
    // transport backlog. Inserting the recovered account tracks its note tag
    // (the store invariant the drain depends on).
    freshDevice();
    const deviceB = await MidenClient.createMock({ serializedNoteTransport: transportState });
    expect(await deviceB.notes.list()).toHaveLength(0);
    await deviceB.accounts.insert({ account: Account.deserialize(accountBytes), overwrite: true });

    const first = await drainPrivateNoteBacklog(deviceB);
    expect(first.status).toBe('completed');
    expect(first.imported).toBe(1);
    expect(first.retryable).toBe(false);
    // Drift guard for the stop signal: the drain reads the per-tag cursors
    // straight from the SDK's IndexedDB store. If an SDK bump renamed the key
    // or moved the table, the read would return null after every fetch and
    // the drain would stop after one fetch while reporting completed.
    expect(await readTransportCursors(deviceB)).not.toBeNull();

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
    await deviceB[Symbol.asyncDispose]();
    freshDevice();
    const blindDevice = await MidenClient.createMock({ serializedNoteTransport: transportState });
    const blind = await drainPrivateNoteBacklog(blindDevice);
    expect(blind.status).toBe('completed');
    expect(blind.imported).toBe(0);
    await blindDevice[Symbol.asyncDispose]();
  }, 120_000);

  it('tracks the standard note tag when a recovered account is inserted', async () => {
    const { MidenClient, Account, NoteTag, AccountId } = await import('@miden-sdk/miden-sdk');

    freshDevice();
    const deviceA = await MidenClient.createMock();
    const account = await deviceA.accounts.create();
    const accountBytes = account.serialize();
    const accountIdHex = account.id().toString();
    await deviceA[Symbol.asyncDispose]();

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
