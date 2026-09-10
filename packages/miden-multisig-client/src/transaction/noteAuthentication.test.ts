import { describe, it, expect, vi, beforeEach } from 'vitest';

import { ensureNotesAuthenticated } from './noteAuthentication.js';
import { ConsumeNoteNotAuthenticatedError } from '../multisig/consumeNotesErrors.js';

const { mockGetNotesById, mockGetRawMidenClient } = vi.hoisted(() => ({
  mockGetNotesById: vi.fn(),
  mockGetRawMidenClient: vi.fn(),
}));

vi.mock('@miden-sdk/miden-sdk', () => ({
  Note: { deserialize: vi.fn() },
  InputNoteState: { Expected: 0, Unverified: 1, Committed: 2, Invalid: 3, ConsumedExternal: 8 },
  NoteFile: {
    fromInputNote: vi.fn((inputNote: unknown) => ({ kind: 'with-proof', inputNote })),
    fromNoteDetails: vi.fn(),
    fromExpectedNote: vi.fn(),
  },
  NoteFilter: vi.fn(),
  NoteFilterTypes: { All: 0 },
  InputNote: {
    authenticated: vi.fn((note: unknown, proof: unknown) => ({ note, proof })),
  },
  NoteDetails: vi.fn(),
  Endpoint: vi.fn().mockImplementation((url: string) => ({ url })),
  RpcClient: vi.fn().mockImplementation(() => ({ getNotesById: mockGetNotesById })),
}));

vi.mock('../raw-client.js', () => ({
  getRawMidenClient: mockGetRawMidenClient,
}));

const NOTE_A = `0x${'aa'.repeat(32)}`;
const NOTE_B = `0x${'bb'.repeat(32)}`;
const MIDEN_RPC_ENDPOINT = 'https://rpc.devnet.miden.io';

function makeNote(idHex: string) {
  return { id: () => ({ toString: () => idHex }) };
}

function makeFetched(idHex: string) {
  return { noteId: { toString: () => idHex }, inclusionProof: `proof:${idHex}` };
}

describe('ensureNotesAuthenticated (issue #409)', () => {
  /** Per-note store state; `undefined` models a record the store never held. */
  let records: Map<string, { authenticated: boolean }>;
  let webClient: {
    getInputNote: ReturnType<typeof vi.fn>;
    importNoteFile: ReturnType<typeof vi.fn>;
    syncState: ReturnType<typeof vi.fn>;
  };

  beforeEach(() => {
    vi.clearAllMocks();
    records = new Map();
    webClient = {
      getInputNote: vi.fn(async (idHex: string) => {
        const record = records.get(idHex);
        return record ? { isAuthenticated: () => record.authenticated } : undefined;
      }),
      // A successful committed import authenticates the record.
      importNoteFile: vi.fn(async (file: { inputNote: { note: { id: () => { toString: () => string } } } }) => {
        records.set(file.inputNote.note.id().toString(), { authenticated: true });
        return 'ok';
      }),
      syncState: vi.fn(async () => undefined),
    };
    mockGetRawMidenClient.mockResolvedValue(webClient);
  });

  function run(notes: ReturnType<typeof makeNote>[]) {
    return ensureNotesAuthenticated({} as never, notes as never, {
      midenRpcEndpoint: MIDEN_RPC_ENDPOINT,
    });
  }

  it('leaves already-authenticated notes alone without contacting the node', async () => {
    records.set(NOTE_A, { authenticated: true });
    await run([makeNote(NOTE_A)]);
    expect(mockGetNotesById).not.toHaveBeenCalled();
    expect(webClient.importNoteFile).not.toHaveBeenCalled();
  });

  it('resolves the raw client with the configured RPC endpoint', async () => {
    // A public MidenClient wrapper needs the endpoint to build its raw client.
    records.set(NOTE_A, { authenticated: true });
    await run([makeNote(NOTE_A)]);
    expect(mockGetRawMidenClient).toHaveBeenCalledWith(expect.anything(), MIDEN_RPC_ENDPOINT);
  });

  it('fetches the proofs of unauthenticated notes in one round trip and imports each as committed', async () => {
    records.set(NOTE_A, { authenticated: false }); // tracked, no proof
    // NOTE_B never seen
    mockGetNotesById.mockResolvedValue([makeFetched(NOTE_A), makeFetched(NOTE_B)]);

    await run([makeNote(NOTE_A), makeNote(NOTE_B)]);

    expect(mockGetNotesById).toHaveBeenCalledTimes(1);
    expect(webClient.importNoteFile).toHaveBeenCalledTimes(2);
    expect(webClient.importNoteFile.mock.calls[0][0]).toEqual({
      kind: 'with-proof',
      inputNote: { note: expect.anything(), proof: `proof:${NOTE_A}` },
    });
    expect(records.get(NOTE_A)?.authenticated).toBe(true);
    expect(records.get(NOTE_B)?.authenticated).toBe(true);
  });

  it('refuses a note the node has no inclusion proof for, naming it', async () => {
    mockGetNotesById.mockResolvedValue([makeFetched(NOTE_A)]);
    const error = await run([makeNote(NOTE_A), makeNote(NOTE_B)]).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ConsumeNoteNotAuthenticatedError);
    expect((error as ConsumeNoteNotAuthenticatedError).noteId).toBe(NOTE_B);
    expect((error as ConsumeNoteNotAuthenticatedError).code).toBe(
      'consume_notes_note_not_authenticated',
    );
    expect((error as Error).message).toContain('not committed on chain yet');
  });

  it('surfaces a proof-fetch failure as a not-authenticated error', async () => {
    mockGetNotesById.mockRejectedValue(new Error('node unreachable'));
    await expect(run([makeNote(NOTE_A)])).rejects.toMatchObject({
      name: 'ConsumeNoteNotAuthenticatedError',
      noteId: NOTE_A,
      message: expect.stringContaining('failed to fetch inclusion proofs'),
    });
    expect(webClient.importNoteFile).not.toHaveBeenCalled();
  });

  it('surfaces an import failure as a not-authenticated error', async () => {
    mockGetNotesById.mockResolvedValue([makeFetched(NOTE_A)]);
    webClient.importNoteFile.mockRejectedValue(new Error('store locked'));
    await expect(run([makeNote(NOTE_A)])).rejects.toMatchObject({
      name: 'ConsumeNoteNotAuthenticatedError',
      noteId: NOTE_A,
      message: expect.stringContaining('failed to import it with its inclusion proof'),
    });
  });

  it('syncs once when an imported note is still unverified, then succeeds', async () => {
    mockGetNotesById.mockResolvedValue([makeFetched(NOTE_A)]);
    // The note's block is newer than the sync height: the import lands
    // unverified and the sync is what verifies it.
    webClient.importNoteFile.mockImplementation(async () => {
      records.set(NOTE_A, { authenticated: false });
      return 'ok';
    });
    webClient.syncState.mockImplementation(async () => {
      records.set(NOTE_A, { authenticated: true });
    });
    await run([makeNote(NOTE_A)]);
    expect(webClient.syncState).toHaveBeenCalledTimes(1);
  });

  it('does not sync when the imports already authenticated every note', async () => {
    mockGetNotesById.mockResolvedValue([makeFetched(NOTE_A)]);
    await run([makeNote(NOTE_A)]);
    expect(webClient.syncState).not.toHaveBeenCalled();
  });

  it('fails closed when the note is still unverified even after a sync', async () => {
    mockGetNotesById.mockResolvedValue([makeFetched(NOTE_A)]);
    webClient.importNoteFile.mockImplementation(async () => {
      records.set(NOTE_A, { authenticated: false });
      return 'ok';
    });
    await expect(run([makeNote(NOTE_A)])).rejects.toMatchObject({
      name: 'ConsumeNoteNotAuthenticatedError',
      noteId: NOTE_A,
      message: expect.stringContaining('even after a sync'),
    });
    expect(webClient.syncState).toHaveBeenCalledTimes(1);
  });
});
