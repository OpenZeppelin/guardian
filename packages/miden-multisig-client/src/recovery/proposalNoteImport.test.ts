import { describe, it, expect, vi, beforeEach } from 'vitest';

import { importNotesFromProposals } from './proposalNoteImport.js';
import type { Proposal } from '../types/proposal.js';
import { uint8ArrayToBase64 } from '../utils/encoding.js';

const { mockNoteDeserialize, mockGetNotesById } = vi.hoisted(() => ({
  mockNoteDeserialize: vi.fn(),
  mockGetNotesById: vi.fn(),
}));

vi.mock('@miden-sdk/miden-sdk', () => ({
  Note: {
    deserialize: mockNoteDeserialize,
  },
  NoteFile: {
    fromInputNote: vi.fn((inputNote: unknown) => ({ kind: 'with-proof', inputNote })),
    fromNoteDetails: vi.fn((details: unknown) => ({ kind: 'details', details })),
    fromExpectedNote: vi.fn((details: unknown, tag: unknown, afterBlockNum: number) => ({
      kind: 'expected',
      details,
      tag,
      afterBlockNum,
    })),
  },
  NoteFilter: vi.fn().mockImplementation((noteType: number) => ({ noteType })),
  NoteFilterTypes: {
    All: 0,
    Consumed: 1,
    Committed: 2,
    Expected: 3,
    Processing: 4,
    List: 5,
    Unique: 6,
    Nullifiers: 7,
    Unverified: 8,
  },
  InputNote: {
    authenticated: vi.fn((note: unknown, proof: unknown) => ({ note, proof })),
  },
  NoteDetails: vi.fn().mockImplementation((assets: unknown, recipient: unknown) => ({
    assets,
    recipient,
  })),
  Endpoint: vi.fn().mockImplementation((url: string) => ({ url })),
  RpcClient: vi.fn().mockImplementation(() => ({
    getNotesById: mockGetNotesById,
  })),
}));

const FILTER_ALL = 0;
const FILTER_CONSUMED = 1;

const MIDEN_RPC_ENDPOINT = 'https://rpc.devnet.miden.io';

const NOTE_ID_1 = `0x${'11'.repeat(32)}`;
const NOTE_ID_2 = `0x${'22'.repeat(32)}`;
const NOTE_ID_3 = `0x${'33'.repeat(32)}`;

/** Base64 whose decoded bytes name the note the mocked `Note.deserialize`
 * should return; unknown names throw, simulating corrupt bytes. */
function noteBase64(name: string): string {
  return uint8ArrayToBase64(new TextEncoder().encode(name));
}

/** Deterministic fake recipient digest for a note id. */
function recipientDigestFor(idHex: string): string {
  return `0xdd${idHex.slice(4)}`;
}

/** Deterministic fake tag (u32) for a note id. */
function tagFor(idHex: string): number {
  return parseInt(idHex.slice(2, 6), 16);
}

/** Deterministic fake fungible-asset list keyed by a note id. */
function noteAssets(idHex: string) {
  const faucetHex = `0xfa${idHex.slice(4, 34)}`;
  return {
    fungibleAssets: () => [
      { faucetId: () => ({ toString: () => faucetHex }), amount: () => 100n },
    ],
  };
}

function makeNote(idHex: string) {
  const assets = noteAssets(idHex);
  return {
    id: () => ({ toString: () => idHex }),
    recipient: () => ({ digest: () => ({ toHex: () => recipientDigestFor(idHex) }) }),
    metadata: () => ({ tag: () => ({ asU32: () => tagFor(idHex) }) }),
    assets: () => assets,
  };
}

/** A store record as returned by `getInputNotes`. `idHex: undefined` models a
 * metadata-less record (expected-state details import or consumed-external),
 * which still exposes its details (recipient digest + assets). `detailsOf`
 * names the note whose details the record shares; `assetsOf` overrides the
 * asset half to model recipient-digest collisions between distinct notes. */
function makeRecord(options: {
  idHex?: string;
  detailsOf?: string;
  assetsOf?: string;
  consumed?: boolean;
}) {
  const detailsSource = options.detailsOf ?? `0x${'ab'.repeat(32)}`;
  const assets = noteAssets(options.assetsOf ?? detailsSource);
  return {
    id: () => (options.idHex ? { toString: () => options.idHex } : undefined),
    details: () => ({
      recipient: () => ({ digest: () => ({ toHex: () => recipientDigestFor(detailsSource) }) }),
      assets: () => assets,
    }),
    isConsumed: () => options.consumed ?? false,
  };
}

function makeFetchedNote(idHex: string, options: { withBody?: boolean } = {}) {
  return {
    noteId: { toString: () => idHex },
    inclusionProof: `proof:${idHex}`,
    // Private notes come back without a body; the import path must never
    // read it.
    note: options.withBody ? makeNote(idHex) : undefined,
  };
}

function makeProposal(
  id: string,
  notes: string[],
  metadataVersion: 1 | 2 = 2,
): Pick<Proposal, 'id' | 'metadata'> {
  return {
    id,
    metadata: {
      proposalType: 'consume_notes',
      description: 'test',
      noteIds: notes.map((_, i) => `0x${String(i).repeat(2)}`),
      ...(metadataVersion === 2 ? { metadataVersion: 2 as const, notes } : {}),
    },
  };
}

describe('importNotesFromProposals', () => {
  let storeRecords: Array<ReturnType<typeof makeRecord>>;
  let consumedRecords: Array<ReturnType<typeof makeRecord>>;
  let mockWebClient: {
    getInputNotes: ReturnType<typeof vi.fn>;
    importNoteFile: ReturnType<typeof vi.fn>;
  };
  const noteRegistry = new Map<string, ReturnType<typeof makeNote>>();

  beforeEach(() => {
    vi.clearAllMocks();
    noteRegistry.clear();
    noteRegistry.set('note-1', makeNote(NOTE_ID_1));
    noteRegistry.set('note-2', makeNote(NOTE_ID_2));
    noteRegistry.set('note-3', makeNote(NOTE_ID_3));
    mockNoteDeserialize.mockImplementation((bytes: Uint8Array) => {
      const name = new TextDecoder().decode(bytes);
      const note = noteRegistry.get(name);
      if (!note) {
        throw new Error(`corrupt note bytes: ${name}`);
      }
      return note;
    });
    storeRecords = [];
    consumedRecords = [];
    mockWebClient = {
      getInputNotes: vi.fn().mockImplementation(async (filter: { noteType: number }) => {
        if (filter.noteType === FILTER_ALL) return storeRecords;
        if (filter.noteType === FILTER_CONSUMED) return consumedRecords;
        return [];
      }),
      importNoteFile: vi.fn().mockResolvedValue(NOTE_ID_1),
    };
  });

  function run(proposals: Array<Pick<Proposal, 'id' | 'metadata'>>) {
    return importNotesFromProposals(mockWebClient as never, proposals, {
      midenRpcEndpoint: MIDEN_RPC_ENDPOINT,
    });
  }

  it('imports a committed public note with its fetched proof', async () => {
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1, { withBody: true })]);

    const outcomes = await run([makeProposal('p-1', [noteBase64('note-1')])]);

    expect(outcomes).toEqual([
      { identifier: NOTE_ID_1, source: 'proposal', status: 'imported' },
    ]);
    expect(mockWebClient.importNoteFile).toHaveBeenCalledWith({
      kind: 'with-proof',
      inputNote: { note: noteRegistry.get('note-1'), proof: `proof:${NOTE_ID_1}` },
    });
  });

  it('imports a committed private note from local bytes only (node has no body)', async () => {
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1, { withBody: false })]);

    const outcomes = await run([makeProposal('p-1', [noteBase64('note-1')])]);

    expect(outcomes[0].status).toBe('imported');
    // The imported note is the one decoded from the proposal bytes, never
    // the (absent) node-returned body.
    expect(mockWebClient.importNoteFile).toHaveBeenCalledWith({
      kind: 'with-proof',
      inputNote: { note: noteRegistry.get('note-1'), proof: `proof:${NOTE_ID_1}` },
    });
  });

  it('parks an uncommitted note as an expected-note file with its sync-hint tag, reported retryable', async () => {
    mockGetNotesById.mockResolvedValue([]);

    const outcomes = await run([makeProposal('p-1', [noteBase64('note-1')])]);

    expect(outcomes).toEqual([
      {
        identifier: NOTE_ID_1,
        source: 'proposal',
        status: 'not-committed',
        retryable: true,
        reason: expect.stringContaining('not yet committed'),
      },
    ]);
    // The tag rides in the expected-note file itself (mirroring the Rust
    // `NoteFile::ExpectedNote` + sync hint), so upstream registers a
    // note-source tag it removes once the note commits — never a permanent
    // user-source `addTag`.
    const expectedFile = mockWebClient.importNoteFile.mock.calls[0][0] as {
      kind: string;
      details: { assets: string; recipient: { digest: () => { toHex: () => string } } };
      tag: { asU32: () => number };
      afterBlockNum: number;
    };
    expect(expectedFile.kind).toBe('expected');
    expect(expectedFile.tag.asU32()).toBe(tagFor(NOTE_ID_1));
    expect(expectedFile.afterBlockNum).toBe(0);
    expect(expectedFile.details.assets).toBe(noteRegistry.get('note-1')!.assets());
    expect(expectedFile.details.recipient.digest().toHex()).toBe(recipientDigestFor(NOTE_ID_1));
  });

  it('isolates a malformed note without blocking the rest', async () => {
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_2)]);

    const outcomes = await run([
      makeProposal('p-9', [noteBase64('garbage'), noteBase64('note-2')]),
    ]);

    expect(outcomes).toEqual([
      {
        identifier: 'proposal p-9 notes[0]',
        source: 'proposal',
        status: 'invalid',
        reason: expect.stringContaining('failed to decode embedded note'),
      },
      { identifier: NOTE_ID_2, source: 'proposal', status: 'imported' },
    ]);
  });

  it('classifies notes the store already tracks, including metadata-less records', async () => {
    storeRecords = [
      // Normal record, matched by note ID.
      makeRecord({ idHex: NOTE_ID_1 }),
      // Metadata-less record (no note ID — e.g. consumed-external or an
      // expected-state details import), matched by recipient digest.
      makeRecord({ detailsOf: NOTE_ID_2, consumed: true }),
    ];
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_3)]);

    const outcomes = await run([
      makeProposal('p-1', [noteBase64('note-1'), noteBase64('note-2'), noteBase64('note-3')]),
    ]);

    expect(outcomes).toEqual([
      { identifier: NOTE_ID_1, source: 'proposal', status: 'already-present' },
      { identifier: NOTE_ID_2, source: 'proposal', status: 'already-consumed' },
      { identifier: NOTE_ID_3, source: 'proposal', status: 'imported' },
    ]);
    // Already-tracked notes are never re-imported.
    expect(mockWebClient.importNoteFile).toHaveBeenCalledTimes(1);
  });

  it('does not mistake a same-recipient record with different assets for the candidate', async () => {
    // Two distinct notes can share a recipient while carrying different
    // assets — recipient digest alone is not collision-safe, so the record
    // must NOT match and the candidate must still be recovered.
    storeRecords = [makeRecord({ detailsOf: NOTE_ID_1, assetsOf: NOTE_ID_2, consumed: true })];
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1)]);

    const outcomes = await run([makeProposal('p-1', [noteBase64('note-1')])]);

    expect(outcomes).toEqual([
      { identifier: NOTE_ID_1, source: 'proposal', status: 'imported' },
    ]);
    expect(mockWebClient.importNoteFile).toHaveBeenCalledTimes(1);
  });

  it('does not relabel an import as consumed for a same-recipient record with different assets', async () => {
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1)]);
    // An unrelated consumed note sharing the recipient but not the assets
    // must not flip the fresh import to already-consumed.
    consumedRecords = [makeRecord({ detailsOf: NOTE_ID_1, assetsOf: NOTE_ID_2, consumed: true })];

    const outcomes = await run([makeProposal('p-1', [noteBase64('note-1')])]);

    expect(outcomes).toEqual([
      { identifier: NOTE_ID_1, source: 'proposal', status: 'imported' },
    ]);
  });

  it('reports a chain-consumed note as already-consumed after importing its history', async () => {
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1)]);
    // The upstream import stores a chain-nullified note as a metadata-less
    // consumed record; the batched post-import check sees it by recipient
    // digest.
    consumedRecords = [
      makeRecord({ detailsOf: NOTE_ID_1, consumed: true }),
    ];

    const outcomes = await run([makeProposal('p-1', [noteBase64('note-1')])]);

    expect(outcomes).toEqual([
      {
        identifier: NOTE_ID_1,
        source: 'proposal',
        status: 'already-consumed',
        reason: expect.stringContaining('already consumed on chain'),
      },
    ]);
    expect(mockWebClient.importNoteFile).toHaveBeenCalledTimes(1);
  });

  it('keeps a successful import as imported when the consumed-state check fails', async () => {
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1)]);
    mockWebClient.getInputNotes.mockImplementation(async (filter: { noteType: number }) => {
      if (filter.noteType === FILTER_ALL) return [];
      throw new Error('store busy');
    });

    const outcomes = await run([makeProposal('p-1', [noteBase64('note-1')])]);

    expect(outcomes).toEqual([
      {
        identifier: NOTE_ID_1,
        source: 'proposal',
        status: 'imported',
        reason: expect.stringContaining('consumed-state check failed'),
      },
    ]);
  });

  it('fails every decoded note when the store scan itself fails', async () => {
    mockWebClient.getInputNotes.mockRejectedValue(new Error('store locked'));

    const outcomes = await run([makeProposal('p-1', [noteBase64('note-1')])]);

    expect(outcomes).toEqual([
      {
        identifier: NOTE_ID_1,
        source: 'proposal',
        status: 'failed',
        reason: expect.stringContaining('failed to read local store'),
      },
    ]);
    expect(mockWebClient.importNoteFile).not.toHaveBeenCalled();
  });

  it('reports a transient proof-fetch failure as retryable for every pending note', async () => {
    mockGetNotesById.mockRejectedValue(Object.assign(new Error('node down'), { code: 14 }));

    const outcomes = await run([
      makeProposal('p-1', [noteBase64('note-1'), noteBase64('note-2')]),
    ]);

    expect(outcomes).toEqual([
      {
        identifier: NOTE_ID_1,
        source: 'proposal',
        status: 'failed',
        retryable: true,
        reason: expect.stringContaining('failed to fetch inclusion proofs'),
      },
      {
        identifier: NOTE_ID_2,
        source: 'proposal',
        status: 'failed',
        retryable: true,
        reason: expect.stringContaining('failed to fetch inclusion proofs'),
      },
    ]);
    expect(mockWebClient.importNoteFile).not.toHaveBeenCalled();
  });

  it('isolates an import failure without blocking the rest, classifying retryability', async () => {
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1), makeFetchedNote(NOTE_ID_2)]);
    mockWebClient.importNoteFile.mockImplementation(
      async (file: { inputNote: { note: unknown } }) => {
        if (file.inputNote.note === noteRegistry.get('note-1')) {
          // Transient node failure surfaced through the import's internal RPC.
          throw Object.assign(new Error('import blip'), { code: 14 });
        }
        return NOTE_ID_2;
      },
    );

    const outcomes = await run([
      makeProposal('p-1', [noteBase64('note-1'), noteBase64('note-2')]),
    ]);

    expect(outcomes).toEqual([
      {
        identifier: NOTE_ID_1,
        source: 'proposal',
        status: 'failed',
        retryable: true,
        reason: expect.stringContaining('failed to import note'),
      },
      { identifier: NOTE_ID_2, source: 'proposal', status: 'imported' },
    ]);
  });

  it('skips v1 proposals and non-consume proposal types', async () => {
    const v1 = makeProposal('p-1', [noteBase64('note-1')], 1);
    const p2id: Pick<Proposal, 'id' | 'metadata'> = {
      id: 'p-2',
      metadata: {
        proposalType: 'p2id',
        description: 'test',
        recipientId: '0xaa',
        faucetId: '0xbb',
        amount: '1',
      },
    };

    const outcomes = await run([v1, p2id]);

    expect(outcomes).toEqual([]);
    expect(mockGetNotesById).not.toHaveBeenCalled();
    expect(mockWebClient.importNoteFile).not.toHaveBeenCalled();
  });

  it('deduplicates the same note embedded by several proposals', async () => {
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1)]);

    const outcomes = await run([
      makeProposal('p-1', [noteBase64('note-1')]),
      makeProposal('p-2', [noteBase64('note-1')]),
    ]);

    expect(outcomes).toEqual([
      { identifier: NOTE_ID_1, source: 'proposal', status: 'imported' },
    ]);
    expect(mockWebClient.importNoteFile).toHaveBeenCalledTimes(1);
  });
});
