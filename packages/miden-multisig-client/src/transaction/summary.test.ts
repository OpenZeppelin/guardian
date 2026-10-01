import { describe, expect, it, vi } from 'vitest';

const {
  mockCaptureAnchor,
  mockPreview,
  mockGetSyncHeight,
  mockSyncChain,
  mockRequestBoundBlockNum,
} = vi.hoisted(() => ({
  mockCaptureAnchor: vi.fn(),
  mockPreview: vi.fn(),
  mockGetSyncHeight: vi.fn(),
  mockSyncChain: vi.fn(),
  mockRequestBoundBlockNum: vi.fn(),
}));

vi.mock('@miden-sdk/miden-sdk', () => ({
  ChainAnchor: { deserialize: vi.fn() },
  Word: {
    newFromFelts: vi.fn((felts: unknown[]) => ({ felts })),
  },
}));

vi.mock('./authArgs.js', () => ({
  requestBoundBlockNum: mockRequestBoundBlockNum,
}));

const {
  ChainBehindBoundBlockError,
  executeForSummary,
  executeForSummaryAt,
  executeForSummaryAtTip,
  isStaleChainError,
  summaryApprovalExpirationBlockNum,
  summarySalt,
  SummaryAnchorMismatchError,
  syncToBoundBlock,
} = await import('./summary.js');
const { BoundBlockNotDeclaredError } = await import('../multisig/authArgErrors.js');

/** A request whose multisig auth args bind `bound` and declare `declared`. */
const requestBinding = (bound: number | undefined, declared: number[] = []) => {
  mockRequestBoundBlockNum.mockReturnValue(bound);
  return { blockNumbers: () => declared } as never;
};

/** The slice of `MidenClient` the summary helpers reach. */
const midenClient = () =>
  ({
    transactions: { captureAnchor: mockCaptureAnchor, preview: mockPreview },
    getSyncHeight: mockGetSyncHeight,
    syncChain: mockSyncChain,
  }) as never;

const ACCOUNT_ID = '0x' + '11'.repeat(15);

const felt = (value: bigint) => ({ asInt: () => value });

describe('summarySalt', () => {
  it('reads the salt from user params two through five', () => {
    const summary = {
      userParams: () => [felt(0n), felt(0n), 11, 22, 33, 44],
    } as never;

    expect(summarySalt(summary)).toEqual({ felts: [11, 22, 33, 44] });
  });

  it('ignores the approval expiration and the zero the auth component puts first', () => {
    const summary = {
      userParams: () => [felt(900n), felt(0n), 11, 22, 33, 44],
    } as never;

    expect(summarySalt(summary)).toEqual({ felts: [11, 22, 33, 44] });
  });
});

describe('summaryApprovalExpirationBlockNum', () => {
  it('reports no expiration for a zero first user param', () => {
    const summary = { userParams: () => [felt(0n), felt(0n), 1, 2, 3, 4] } as never;

    expect(summaryApprovalExpirationBlockNum(summary)).toBeUndefined();
  });

  it('reads the expiration block from the first user param', () => {
    const summary = { userParams: () => [felt(1234n), felt(0n), 1, 2, 3, 4] } as never;

    expect(summaryApprovalExpirationBlockNum(summary)).toBe(1234);
  });
});

describe('executeForSummary', () => {
  const anchorWith = (commitmentHex: string) => ({
    commitment: () => ({ toHex: () => commitmentHex }),
    free: vi.fn(),
  });
  const summaryBinding = (commitmentHex: string) => ({
    blockCommitment: () => ({ toHex: () => commitmentHex }),
  });

  it('derives the summary at the tip and returns the anchor naming the bound block', async () => {
    const anchor = anchorWith('0x' + 'ab'.repeat(32));
    mockCaptureAnchor.mockResolvedValue(anchor);
    mockGetSyncHeight.mockResolvedValue(40);
    mockPreview.mockResolvedValue(summaryBinding('0x' + 'AB'.repeat(32)));
    const request = requestBinding(40, [40]);

    const result = await executeForSummary(midenClient(), ACCOUNT_ID, request);

    expect(result.anchor).toBe(anchor);
    expect(anchor.free).not.toHaveBeenCalled();
    expect(mockCaptureAnchor).toHaveBeenCalledWith(request);
    // A multisig proposal is never re-executed at an anchor.
    expect(mockPreview).toHaveBeenCalledTimes(1);
    expect(mockPreview.mock.calls[0]?.[0]).toStrictEqual({
      operation: 'custom',
      account: ACCOUNT_ID,
      request,
    });
  });

  it('frees the anchor and fails when the summary binds another block', async () => {
    const anchor = anchorWith('0x' + 'ab'.repeat(32));
    mockCaptureAnchor.mockResolvedValue(anchor);
    mockGetSyncHeight.mockResolvedValue(41);
    mockPreview.mockResolvedValue(summaryBinding('0x' + 'cd'.repeat(32)));

    const attempt = executeForSummary(midenClient(), ACCOUNT_ID, requestBinding(40, [40]));

    await expect(attempt).rejects.toBeInstanceOf(SummaryAnchorMismatchError);
    await expect(attempt).rejects.toMatchObject({ retryable: true });
    expect(anchor.free).toHaveBeenCalledTimes(1);
  });

  it('frees the anchor when execution itself fails', async () => {
    const anchor = anchorWith('0x' + 'ab'.repeat(32));
    mockCaptureAnchor.mockResolvedValue(anchor);
    mockGetSyncHeight.mockResolvedValue(40);
    mockPreview.mockRejectedValue(new Error('boom'));

    await expect(
      executeForSummary(midenClient(), ACCOUNT_ID, requestBinding(40, [40])),
    ).rejects.toThrow('boom');
    expect(anchor.free).toHaveBeenCalledTimes(1);
  });

  it('fails without executing when the anchor cannot be captured', async () => {
    mockCaptureAnchor.mockRejectedValue(new Error('INVALID_CHAIN_ANCHOR'));

    await expect(
      executeForSummary(midenClient(), ACCOUNT_ID, requestBinding(40, [40])),
    ).rejects.toThrow('INVALID_CHAIN_ANCHOR');
    expect(mockPreview).not.toHaveBeenCalled();
  });
});

describe('executeForSummaryAt', () => {
  it('previews the request at the anchor it is given', async () => {
    const anchor = { marker: 'anchor' };
    const summary = { marker: 'summary' };
    const request = requestBinding(undefined);
    mockPreview.mockResolvedValue(summary);

    await expect(
      executeForSummaryAt(midenClient(), ACCOUNT_ID, request, anchor as never),
    ).resolves.toBe(summary);
    expect(mockPreview).toHaveBeenCalledWith({
      operation: 'custom',
      account: ACCOUNT_ID,
      request,
      anchor,
    });
    expect(mockSyncChain).not.toHaveBeenCalled();
  });

  it('reports a client it cannot use as a rejection, like the other helpers', async () => {
    const attempt = executeForSummaryAt({} as never, ACCOUNT_ID, requestBinding(undefined), {} as never);

    await expect(attempt).rejects.toBeInstanceOf(TypeError);
  });
});

describe('executeForSummaryAtTip', () => {
  const summary = { marker: 'summary' };

  it('executes without syncing once the client has reached the bound block', async () => {
    mockGetSyncHeight.mockResolvedValue(95);
    mockPreview.mockResolvedValue(summary);

    await expect(
      executeForSummaryAtTip(midenClient(), ACCOUNT_ID, requestBinding(40, [40])),
    ).resolves.toBe(summary);
    expect(mockSyncChain).not.toHaveBeenCalled();
    expect(mockPreview.mock.calls[0]?.[0]).not.toHaveProperty('anchor');
  });

  it('syncs the chain once when the client is still below the bound block', async () => {
    mockGetSyncHeight.mockResolvedValueOnce(30).mockResolvedValueOnce(42);
    mockSyncChain.mockResolvedValue({});
    mockPreview.mockResolvedValue(summary);

    await expect(
      executeForSummaryAtTip(midenClient(), ACCOUNT_ID, requestBinding(40, [40])),
    ).resolves.toBe(summary);
    expect(mockSyncChain).toHaveBeenCalledTimes(1);
    expect(mockSyncChain.mock.invocationCallOrder[0]).toBeLessThan(
      mockPreview.mock.invocationCallOrder[0],
    );
  });

  it('refuses to execute when the node has not reached the bound block', async () => {
    mockGetSyncHeight.mockResolvedValue(30);
    mockSyncChain.mockResolvedValue({});

    const attempt = executeForSummaryAtTip(midenClient(), ACCOUNT_ID, requestBinding(40, [40]));

    await expect(attempt).rejects.toThrow('synced to block 30, below block 40');
    // A lagging node clears on its own, so the failure is worth retrying.
    await expect(attempt).rejects.toBeInstanceOf(ChainBehindBoundBlockError);
    await expect(attempt).rejects.toMatchObject({ retryable: true, syncHeight: 30, boundBlockNum: 40 });
    expect(mockPreview).not.toHaveBeenCalled();
  });

  it('refuses a request that binds a block without declaring it', async () => {
    const attempt = executeForSummaryAtTip(midenClient(), ACCOUNT_ID, requestBinding(40, []));

    await expect(attempt).rejects.toBeInstanceOf(BoundBlockNotDeclaredError);
    await expect(attempt).rejects.toMatchObject({ boundBlockNum: 40 });
    expect(mockPreview).not.toHaveBeenCalled();
  });

  it('executes a request without multisig auth args as is', async () => {
    mockPreview.mockResolvedValue(summary);

    await expect(
      executeForSummaryAtTip(midenClient(), ACCOUNT_ID, requestBinding(undefined)),
    ).resolves.toBe(summary);
    expect(mockGetSyncHeight).not.toHaveBeenCalled();
  });
});


describe('syncToBoundBlock', () => {
  it('syncs through the caller-supplied sync, so a retry policy can wrap it', async () => {
    mockGetSyncHeight.mockResolvedValueOnce(10).mockResolvedValueOnce(12);
    const retried = vi.fn().mockResolvedValue(undefined);

    await syncToBoundBlock(midenClient(), 12, retried);

    expect(retried).toHaveBeenCalledTimes(1);
    expect(mockSyncChain).not.toHaveBeenCalled();
  });

  it('defaults to a chain sync, which leaves the note transport alone', async () => {
    mockGetSyncHeight.mockResolvedValueOnce(10).mockResolvedValueOnce(12);
    mockSyncChain.mockResolvedValue({});

    await syncToBoundBlock(midenClient(), 12);

    expect(mockSyncChain).toHaveBeenCalledTimes(1);
  });

  it('does not sync a client already at or past the bound block', async () => {
    mockGetSyncHeight.mockResolvedValue(12);

    await syncToBoundBlock(midenClient(), 12);

    expect(mockSyncChain).not.toHaveBeenCalled();
  });
});

describe('isStaleChainError', () => {
  it('treats pruned account state and a lagging node as chain state to catch up with', () => {
    expect(isStaleChainError(new Error('grpc get_account: block 144937 has been pruned'))).toBe(true);
    expect(isStaleChainError(new ChainBehindBoundBlockError({ syncHeight: 1, boundBlockNum: 2 }))).toBe(true);
  });

  it('does not excuse a proposal that cannot be reproduced', () => {
    expect(isStaleChainError(new Error('Invalid proposal: metadata does not match tx_summary'))).toBe(false);
  });
});
