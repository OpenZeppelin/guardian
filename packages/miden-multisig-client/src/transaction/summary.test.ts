import { describe, expect, it, vi } from 'vitest';

const {
  mockPreview,
  mockGetSyncHeight,
  mockSyncChain,
  mockRequestBoundBlockNum,
} = vi.hoisted(() => ({
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
  executeForSummaryAtTip,
  isStaleChainError,
  legacyChainAnchorBlockNum,
  summaryApprovalExpirationBlockNum,
  summarySalt,
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
    transactions: { preview: mockPreview },
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

describe('legacyChainAnchorBlockNum', () => {
  it('reads the block a legacy anchor names and frees the anchor', async () => {
    const { ChainAnchor } = await import('@miden-sdk/miden-sdk');
    const free = vi.fn();
    vi.mocked(ChainAnchor.deserialize).mockReturnValueOnce({ blockNum: () => 77, free } as never);

    expect(legacyChainAnchorBlockNum('AQID')).toBe(77);
    expect(free).toHaveBeenCalledTimes(1);
  });
});
