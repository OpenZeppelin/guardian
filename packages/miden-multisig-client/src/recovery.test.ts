import { describe, it, expect, vi } from 'vitest';
import type { MidenClient } from '@miden-sdk/miden-sdk';
import { freshDeviceStore as freshDevice } from './testing/fake-indexeddb-device.js';
import { drainPrivateNoteBacklog } from './recovery.js';

/**
 * Classification tests use hand-rolled stub clients — the primitive takes the
 * `MidenClient` as an argument, so no module mocking is needed. Behavioral
 * tests (further down) run against the real WASM mock client.
 */
function stubClient(options: {
  listLengths: number[];
  fetchPrivate: (opts?: { mode?: string }) => Promise<void>;
}): { client: MidenClient; fetchPrivate: ReturnType<typeof vi.fn> } {
  let call = 0;
  const fetchPrivate = vi.fn(options.fetchPrivate);
  const client = {
    notes: {
      list: vi.fn(async () => {
        const length = options.listLengths[Math.min(call, options.listLengths.length - 1)];
        call += 1;
        return new Array(length).fill({});
      }),
      fetchPrivate,
    },
  } as unknown as MidenClient;
  return { client, fetchPrivate };
}

describe('drainPrivateNoteBacklog', () => {
  it('reports completed with the count of newly imported records', async () => {
    const { client, fetchPrivate } = stubClient({
      listLengths: [1, 3],
      fetchPrivate: async () => {},
    });

    const report = await drainPrivateNoteBacklog(client);

    expect(fetchPrivate).toHaveBeenCalledWith({ mode: 'all' });
    expect(report).toEqual({ status: 'completed', imported: 2, retryable: false });
  });

  it('reports a disabled transport as unavailable, not retryable, without throwing', async () => {
    const { client } = stubClient({
      listLengths: [0, 0],
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

  it('reports an unreachable transport as unavailable and retryable', async () => {
    const { client } = stubClient({
      listLengths: [0, 0],
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
      listLengths: [0, 5],
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
      listLengths: [0, 0],
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
      listLengths: [0, 0],
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
});

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
    const note = await createP2IDNote({
      from: account,
      to: account,
      assets: [],
      type: NoteVisibility.Private,
    });
    await deviceA.notes.sendPrivate({ note, to: account });
    const transportState = await deviceA.serializeMockNoteTransportNode();

    // Device B ("new device after loss"): fresh store sharing the same
    // transport backlog. Loading the recovered account tracks its note tag
    // (the load/tag invariant the drain depends on).
    freshDevice();
    const deviceB = await MidenClient.createMock({ serializedNoteTransport: transportState });
    expect(await deviceB.notes.list()).toHaveLength(0);
    await deviceB.accounts.insert({ account: Account.deserialize(accountBytes), overwrite: true });

    const first = await drainPrivateNoteBacklog(deviceB);
    expect(first.status).toBe('completed');
    expect(first.imported).toBe(1);
    expect(first.retryable).toBe(false);

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

  it('tracks the standard note tag when a recovered account is inserted (load path)', async () => {
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
