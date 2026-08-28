import { describe, it, expect, vi, beforeEach } from 'vitest';

import { backfillPublicNotesByTag, type BackfillPublicNotesOptions } from './publicNoteBackfill.js';

const { mockSyncNotes, mockGetNotesById, mockGetBlockHeaderByNumber } = vi.hoisted(() => ({
  mockSyncNotes: vi.fn(),
  mockGetNotesById: vi.fn(),
  mockGetBlockHeaderByNumber: vi.fn(),
}));

vi.mock('@miden-sdk/miden-sdk', () => ({
  InputNoteState: {
    Expected: 0,
    Unverified: 1,
    Committed: 2,
    Invalid: 3,
    ConsumedExternal: 8,
  },
  AccountId: {
    fromHex: vi.fn((hex: string) => {
      if (!hex.startsWith('0x')) {
        throw new Error(`invalid account id: ${hex}`);
      }
      return {
        hex,
        prefix: () => ({ asInt: () => BigInt('0x' + hex.slice(2, 10)) }),
        suffix: () => ({ asInt: () => BigInt('0x' + hex.slice(-8)) }),
      };
    }),
  },
  NoteScript: {
    p2id: vi.fn(() => ({ root: () => ({ toHex: () => '0x' + 'a1'.repeat(32) }) })),
    p2ide: vi.fn(() => ({ root: () => ({ toHex: () => '0x' + 'a2'.repeat(32) }) })),
  },
  NoteTag: {
    withAccountTarget: vi.fn((accountId: { hex: string }) => ({ target: accountId.hex })),
  },
  NoteType: { Private: 0, Public: 1 },
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
  NoteFile: {
    fromInputNote: vi.fn((inputNote: unknown) => ({ kind: 'with-proof', inputNote })),
  },
  Endpoint: vi.fn().mockImplementation((url: string) => ({ url })),
  RpcClient: vi.fn().mockImplementation(() => ({
    syncNotes: mockSyncNotes,
    getNotesById: mockGetNotesById,
    getBlockHeaderByNumber: mockGetBlockHeaderByNumber,
  })),
}));

const FILTER_ALL = 0;
const FILTER_CONSUMED = 1;

const MIDEN_RPC_ENDPOINT = 'https://rpc.devnet.miden.io';
const ACCOUNT_ID = `0x${'7b'.repeat(15)}`;

const NOTE_ID_1 = `0x${'11'.repeat(32)}`;
const NOTE_ID_2 = `0x${'22'.repeat(32)}`;
const NOTE_ID_3 = `0x${'33'.repeat(32)}`;

const OTHER_ACCOUNT_ID = `0x${'5c'.repeat(15)}`;
const P2ID_ROOT = `0x${'a1'.repeat(32)}`;
const UNKNOWN_ROOT = `0x${'ee'.repeat(32)}`;

const PAGINATION_ERROR = () =>
  new Error('rpc pagination error: too many pagination iterations, possible infinite loop');
const TRANSIENT_ERROR = () => Object.assign(new Error('node down'), { code: 14 });

/** Deterministic fake recipient digest for a note id. */
function recipientDigestFor(idHex: string): string {
  return `0xdd${idHex.slice(4)}`;
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

function makeNote(
  idHex: string,
  options: { scriptRoot?: string; targetHex?: string } = {},
) {
  const assets = noteAssets(idHex);
  const scriptRoot = options.scriptRoot ?? P2ID_ROOT;
  const targetHex = options.targetHex ?? ACCOUNT_ID;
  // P2ID note storage layout: [target.suffix, target.prefix].
  const storageItems = [
    { asInt: () => BigInt('0x' + targetHex.slice(-8)) },
    { asInt: () => BigInt('0x' + targetHex.slice(2, 10)) },
  ];
  return {
    id: () => ({ toString: () => idHex }),
    script: () => ({ root: () => ({ toHex: () => scriptRoot }) }),
    recipient: () => ({
      digest: () => ({ toHex: () => recipientDigestFor(idHex) }),
      storage: () => ({ items: () => storageItems }),
    }),
    assets: () => assets,
  };
}

/** A committed note as surfaced by the `syncNotes` scan (header data only). */
function makeCommitted(idHex: string, noteType: number) {
  return {
    noteId: () => ({ toString: () => idHex }),
    noteType: () => noteType,
  };
}

function syncInfoWith(...committed: Array<ReturnType<typeof makeCommitted>>) {
  return { notes: () => committed };
}

function makeFetchedNote(
  idHex: string,
  options: { withBody?: boolean; scriptRoot?: string; targetHex?: string } = {},
) {
  return {
    noteId: { toString: () => idHex },
    inclusionProof: `proof:${idHex}`,
    note: (options.withBody ?? true) ? makeNote(idHex, options) : undefined,
  };
}

/** A store record as returned by `getInputNotes`; `idHex: undefined` models a
 * metadata-less record (details import or consumed-external) matched by its
 * details instead, and `proofBacked: false` models an expected record that
 * never received its inclusion proof. */
function makeRecord(options: {
  idHex?: string;
  detailsOf?: string;
  consumed?: boolean;
  proofBacked?: boolean;
}) {
  const detailsSource = options.detailsOf ?? `0x${'ab'.repeat(32)}`;
  const assets = noteAssets(detailsSource);
  return {
    id: () => (options.idHex ? { toString: () => options.idHex } : undefined),
    details: () => ({
      recipient: () => ({ digest: () => ({ toHex: () => recipientDigestFor(detailsSource) }) }),
      assets: () => assets,
    }),
    isConsumed: () => options.consumed ?? false,
    // Mirrors the WASM InputNoteState enum: Committed = 2.
    state: () => 2,
    inclusionProof: () =>
      (options.proofBacked ?? true) ? `stored-proof:${detailsSource}` : undefined,
  };
}

describe('backfillPublicNotesByTag', () => {
  let storeRecords: Array<ReturnType<typeof makeRecord>>;
  let consumedRecords: Array<ReturnType<typeof makeRecord>>;
  let mockWebClient: {
    getInputNotes: ReturnType<typeof vi.fn>;
    importNoteFile: ReturnType<typeof vi.fn>;
  };

  beforeEach(() => {
    vi.clearAllMocks();
    storeRecords = [];
    consumedRecords = [];
    mockWebClient = {
      getInputNotes: vi.fn().mockImplementation(async (filter: { noteType: number }) => {
        if (filter.noteType === FILTER_ALL) return [...storeRecords, ...consumedRecords];
        if (filter.noteType === FILTER_CONSUMED) return consumedRecords;
        return [];
      }),
      importNoteFile: vi.fn().mockResolvedValue(undefined),
    };
    mockGetBlockHeaderByNumber.mockResolvedValue({ blockNum: () => 42 });
    mockSyncNotes.mockResolvedValue(syncInfoWith());
    mockGetNotesById.mockResolvedValue([]);
  });

  function run(overrides: Partial<BackfillPublicNotesOptions> = {}) {
    return backfillPublicNotesByTag(mockWebClient as never, {
      accountId: ACCOUNT_ID,
      midenRpcEndpoint: MIDEN_RPC_ENDPOINT,
      ...overrides,
    });
  }

  it('discovers a public note behind the cursor and imports it with its proof', async () => {
    mockSyncNotes.mockResolvedValue(syncInfoWith(makeCommitted(NOTE_ID_1, 1)));
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1)]);

    const report = await run();

    expect(report).toEqual({
      scannedFrom: 0,
      scannedTo: 42,
      discovered: 1,
      skippedPrivate: 0,
      skippedIrrelevant: 0,
      skippedUnscreenable: 0,
      outcomes: [{ identifier: NOTE_ID_1, source: 'backfill', status: 'imported' }],
      uncovered: [],
      retryable: false,
    });
    // The scan covered the default range with the account's standard tag.
    expect(mockSyncNotes).toHaveBeenCalledTimes(1);
    expect(mockSyncNotes).toHaveBeenCalledWith(0, 42, [{ target: ACCOUNT_ID }]);
    expect(mockWebClient.importNoteFile).toHaveBeenCalledWith({
      kind: 'with-proof',
      inputNote: expect.objectContaining({ proof: `proof:${NOTE_ID_1}` }),
    });
  });

  it('honors an explicit block range without consulting the chain tip', async () => {
    const report = await run({ fromBlock: 10, toBlock: 20 });

    expect(mockGetBlockHeaderByNumber).not.toHaveBeenCalled();
    expect(mockSyncNotes).toHaveBeenCalledWith(10, 20, [{ target: ACCOUNT_ID }]);
    expect(report.scannedFrom).toBe(10);
    expect(report.scannedTo).toBe(20);
  });

  it('deduplicates repeated discoveries and skips private matches', async () => {
    mockSyncNotes.mockResolvedValue(
      syncInfoWith(
        makeCommitted(NOTE_ID_1, 1),
        // The same note surfacing again (tags are best-effort filters and
        // scans may overlap) folds into one outcome.
        makeCommitted(NOTE_ID_1, 1),
        // A private match: discovered, but the chain holds no body for it.
        makeCommitted(NOTE_ID_2, 0),
      ),
    );
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1)]);

    const report = await run();

    expect(report.discovered).toBe(2);
    expect(report.skippedPrivate).toBe(1);
    expect(report.outcomes).toEqual([
      { identifier: NOTE_ID_1, source: 'backfill', status: 'imported' },
    ]);
    expect(mockGetNotesById).toHaveBeenCalledTimes(1);
    expect(mockGetNotesById.mock.calls[0][0]).toHaveLength(1);
  });

  it('screens out tag-colliding notes the account cannot consume, like normal sync', async () => {
    mockSyncNotes.mockResolvedValue(
      syncInfoWith(makeCommitted(NOTE_ID_1, 1), makeCommitted(NOTE_ID_2, 1), makeCommitted(NOTE_ID_3, 1)),
    );
    mockGetNotesById.mockResolvedValue([
      makeFetchedNote(NOTE_ID_1),
      // A real P2ID note addressed at a different account (tag collision).
      makeFetchedNote(NOTE_ID_2, { targetHex: OTHER_ACCOUNT_ID }),
      // A note with an unknown script: not statically screenable, so it is
      // conservatively not imported — but counted apart from the screened-out
      // one so the two classes stay distinguishable.
      makeFetchedNote(NOTE_ID_3, { scriptRoot: UNKNOWN_ROOT }),
    ]);

    const report = await run();

    expect(report.discovered).toBe(3);
    expect(report.skippedIrrelevant).toBe(1);
    expect(report.skippedUnscreenable).toBe(1);
    expect(report.outcomes).toEqual([
      { identifier: NOTE_ID_1, source: 'backfill', status: 'imported' },
    ]);
    expect(mockWebClient.importNoteFile).toHaveBeenCalledTimes(1);
  });

  it('skips notes the store already tracks, including metadata-less records', async () => {
    mockSyncNotes.mockResolvedValue(
      syncInfoWith(makeCommitted(NOTE_ID_1, 1), makeCommitted(NOTE_ID_2, 1)),
    );
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1), makeFetchedNote(NOTE_ID_2)]);
    storeRecords = [
      // Tracked normally by ID: skipped without re-importing.
      makeRecord({ idHex: NOTE_ID_1, detailsOf: NOTE_ID_1 }),
      // A metadata-less consumed-external record: a details-key match is a
      // lossy approximation, so the candidate is NOT pre-skipped — it
      // re-imports (the upstream import dedupes exactly) and the
      // post-import check classifies it from the record.
      makeRecord({ detailsOf: NOTE_ID_2, consumed: true }),
    ];

    const report = await run();

    expect(report.outcomes).toEqual([
      { identifier: NOTE_ID_1, source: 'backfill', status: 'already-present' },
      {
        identifier: NOTE_ID_2,
        source: 'backfill',
        status: 'already-consumed',
        reason: expect.stringContaining('already consumed on chain'),
      },
    ]);
    // The ID-matched record is never re-imported; the details-key match is.
    expect(mockWebClient.importNoteFile).toHaveBeenCalledTimes(1);
  });

  it('upgrades a proof-less expected record with the fetched proof instead of skipping it', async () => {
    mockSyncNotes.mockResolvedValue(syncInfoWith(makeCommitted(NOTE_ID_1, 1)));
    // The unknown script would fail the relevance screen — but tracked
    // records are material the user already chose to track, so the upgrade
    // path must bypass screening.
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1, { scriptRoot: UNKNOWN_ROOT })]);
    // An expected record without a proof (e.g. left by a proposal import
    // that ran while the note was uncommitted): forward sync will never
    // revisit the note's block, so the backfill must apply the proof.
    storeRecords = [makeRecord({ detailsOf: NOTE_ID_1, proofBacked: false })];

    const report = await run();

    expect(report.outcomes).toEqual([
      { identifier: NOTE_ID_1, source: 'backfill', status: 'imported' },
    ]);
    expect(mockWebClient.importNoteFile).toHaveBeenCalledTimes(1);
  });

  it('rebuilds fresh NoteId handles for each body-fetch retry attempt', async () => {
    mockSyncNotes.mockResolvedValue(syncInfoWith(makeCommitted(NOTE_ID_1, 1)));
    // First attempt fails transiently; the retry must succeed — which it can
    // only do if the closure minted fresh NoteId handles (the WASM bridge
    // consumes call arguments, so reusing them would poison the retry).
    mockGetNotesById
      .mockRejectedValueOnce(TRANSIENT_ERROR())
      .mockResolvedValueOnce([makeFetchedNote(NOTE_ID_1)]);

    const report = await run();

    expect(report.outcomes).toEqual([
      { identifier: NOTE_ID_1, source: 'backfill', status: 'imported' },
    ]);
    expect(mockGetNotesById).toHaveBeenCalledTimes(2);
    const firstAttemptIds = mockGetNotesById.mock.calls[0][0];
    const secondAttemptIds = mockGetNotesById.mock.calls[1][0];
    expect(firstAttemptIds).toHaveLength(1);
    expect(secondAttemptIds).toHaveLength(1);
    expect(secondAttemptIds[0]).not.toBe(firstAttemptIds[0]);
  });

  it('reclassifies an import the chain had already nullified as already-consumed', async () => {
    mockSyncNotes.mockResolvedValue(syncInfoWith(makeCommitted(NOTE_ID_1, 1)));
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1)]);
    consumedRecords = [makeRecord({ detailsOf: NOTE_ID_1, consumed: true })];

    const report = await run();

    expect(report.outcomes).toEqual([
      {
        identifier: NOTE_ID_1,
        source: 'backfill',
        status: 'already-consumed',
        reason: 'note was already consumed on chain; recorded as consumption history',
      },
    ]);
  });

  it('splits the range around the pagination cap and still recovers the note', async () => {
    mockSyncNotes.mockImplementation(async (lo: number, hi: number) => {
      if (hi - lo + 1 > 3) {
        throw PAGINATION_ERROR();
      }
      return lo === 0 ? syncInfoWith(makeCommitted(NOTE_ID_1, 1)) : syncInfoWith();
    });
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1)]);

    const report = await run({ fromBlock: 0, toBlock: 9 });

    expect(report.uncovered).toEqual([]);
    expect(report.retryable).toBe(false);
    expect(report.reason).toBeUndefined();
    expect(report.outcomes).toEqual([
      { identifier: NOTE_ID_1, source: 'backfill', status: 'imported' },
    ]);
    expect(mockSyncNotes.mock.calls.length).toBeGreaterThan(1);
  });

  it('reports unsplittable pagination failures as uncovered instead of failing', async () => {
    mockSyncNotes.mockRejectedValue(PAGINATION_ERROR());

    const report = await run({ fromBlock: 0, toBlock: 3 });

    expect(report.discovered).toBe(0);
    expect(report.outcomes).toEqual([]);
    expect(report.uncovered).toEqual([
      { from: 0, to: 0 },
      { from: 1, to: 1 },
      { from: 2, to: 2 },
      { from: 3, to: 3 },
    ]);
    expect(report.reason).toBeDefined();
  });

  it('reports a failing scan as an uncovered retryable range instead of throwing', async () => {
    mockSyncNotes.mockRejectedValue(TRANSIENT_ERROR());

    const report = await run({ fromBlock: 0, toBlock: 9 });

    expect(report.uncovered).toEqual([{ from: 0, to: 9 }]);
    expect(report.retryable).toBe(true);
    expect(report.reason).toContain('blocks [0, 9]');
    expect(report.outcomes).toEqual([]);
  });

  it('classifies a transient body-fetch failure as retryable for every note in the chunk', async () => {
    mockSyncNotes.mockResolvedValue(
      syncInfoWith(makeCommitted(NOTE_ID_1, 1), makeCommitted(NOTE_ID_2, 1)),
    );
    mockGetNotesById.mockRejectedValue(TRANSIENT_ERROR());

    const report = await run();

    expect(report.outcomes).toEqual([
      {
        identifier: NOTE_ID_1,
        source: 'backfill',
        status: 'failed',
        retryable: true,
        reason: expect.stringContaining('failed to fetch note bodies'),
      },
      {
        identifier: NOTE_ID_2,
        source: 'backfill',
        status: 'failed',
        retryable: true,
        reason: expect.stringContaining('failed to fetch note bodies'),
      },
    ]);
    // Retryable outcomes surface at report level too, so orchestration keyed
    // on the report alone knows a rerun can help.
    expect(report.retryable).toBe(true);
    expect(mockWebClient.importNoteFile).not.toHaveBeenCalled();
  });

  it('reports a public note the node returned without a body', async () => {
    mockSyncNotes.mockResolvedValue(syncInfoWith(makeCommitted(NOTE_ID_3, 1)));
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_3, { withBody: false })]);

    const report = await run();

    expect(report.outcomes).toEqual([
      {
        identifier: NOTE_ID_3,
        source: 'backfill',
        status: 'failed',
        retryable: true,
        reason: 'the node did not return a body for this public note',
      },
    ]);
  });

  it('reports a store read failure per note without throwing', async () => {
    mockSyncNotes.mockResolvedValue(syncInfoWith(makeCommitted(NOTE_ID_1, 1)));
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1)]);
    mockWebClient.getInputNotes.mockRejectedValue(new Error('idb exploded'));

    const report = await run();

    expect(report.outcomes).toEqual([
      {
        identifier: NOTE_ID_1,
        source: 'backfill',
        status: 'failed',
        reason: expect.stringContaining('failed to read local store'),
      },
    ]);
  });

  it('isolates an import failure without blocking the rest', async () => {
    mockSyncNotes.mockResolvedValue(
      syncInfoWith(makeCommitted(NOTE_ID_1, 1), makeCommitted(NOTE_ID_2, 1)),
    );
    mockGetNotesById.mockResolvedValue([makeFetchedNote(NOTE_ID_1), makeFetchedNote(NOTE_ID_2)]);
    mockWebClient.importNoteFile.mockImplementation(
      async (file: { inputNote: { proof: string } }) => {
        if (file.inputNote.proof === `proof:${NOTE_ID_1}`) {
          throw new Error('import rejected');
        }
      },
    );

    const report = await run();

    expect(report.outcomes).toEqual([
      {
        identifier: NOTE_ID_1,
        source: 'backfill',
        status: 'failed',
        retryable: false,
        reason: expect.stringContaining('failed to import note'),
      },
      { identifier: NOTE_ID_2, source: 'backfill', status: 'imported' },
    ]);
  });

  it('throws when the chain tip cannot be resolved and no toBlock was given', async () => {
    mockGetBlockHeaderByNumber.mockRejectedValue(new Error('tip lookup failed'));

    await expect(run()).rejects.toThrow('failed to resolve the chain tip');
    expect(mockSyncNotes).not.toHaveBeenCalled();
  });

  it('throws on an inverted range', async () => {
    await expect(run({ fromBlock: 5, toBlock: 1 })).rejects.toThrow('inverted');
    expect(mockSyncNotes).not.toHaveBeenCalled();
  });

  it('throws on a malformed account id before any network work', async () => {
    await expect(run({ accountId: 'not-hex' })).rejects.toThrow('invalid account id');
    expect(mockGetBlockHeaderByNumber).not.toHaveBeenCalled();
    expect(mockSyncNotes).not.toHaveBeenCalled();
  });
});
