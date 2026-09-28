import { describe, expect, it, vi } from 'vitest';

const {
  mockChainAnchorForRequest,
  mockExecuteForSummary,
  mockExecuteForSummaryAt,
  mockGetSyncHeight,
  mockSyncState,
  mockRequestBoundBlockNum,
} = vi.hoisted(() => ({
  mockChainAnchorForRequest: vi.fn(),
  mockExecuteForSummary: vi.fn(),
  mockExecuteForSummaryAt: vi.fn(),
  mockGetSyncHeight: vi.fn(),
  mockSyncState: vi.fn(),
  mockRequestBoundBlockNum: vi.fn(),
}));

vi.mock('@miden-sdk/miden-sdk', () => ({
  AccountId: { fromHex: vi.fn((hex: string) => ({ hex })) },
  ChainAnchor: { deserialize: vi.fn() },
  Word: {
    newFromFelts: vi.fn((felts: unknown[]) => ({ felts })),
  },
}));

vi.mock('../raw-client.js', () => ({
  getRawMidenClient: vi.fn(async () => ({
    chainAnchorForRequest: mockChainAnchorForRequest,
    executeForSummary: mockExecuteForSummary,
    executeForSummaryAt: mockExecuteForSummaryAt,
    getSyncHeight: mockGetSyncHeight,
    syncState: mockSyncState,
  })),
}));

vi.mock('./authArgs.js', () => ({
  requestBoundBlockNum: mockRequestBoundBlockNum,
}));

const {
  ChainBehindBoundBlockError,
  executeForSummary,
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
    mockChainAnchorForRequest.mockResolvedValue(anchor);
    mockGetSyncHeight.mockResolvedValue(40);
    mockExecuteForSummary.mockResolvedValue(summaryBinding('0x' + 'AB'.repeat(32)));

    const result = await executeForSummary(
      {} as never,
      '0x' + '11'.repeat(15),
      requestBinding(40, [40]),
    );

    expect(result.anchor).toBe(anchor);
    expect(anchor.free).not.toHaveBeenCalled();
    // A multisig proposal is never re-executed at an anchor.
    expect(mockExecuteForSummary).toHaveBeenCalledTimes(1);
    expect(mockExecuteForSummaryAt).not.toHaveBeenCalled();
  });

  it('frees the anchor and fails when the summary binds another block', async () => {
    const anchor = anchorWith('0x' + 'ab'.repeat(32));
    mockChainAnchorForRequest.mockResolvedValue(anchor);
    mockGetSyncHeight.mockResolvedValue(41);
    mockExecuteForSummary.mockResolvedValue(summaryBinding('0x' + 'cd'.repeat(32)));

    const attempt = executeForSummary(
      {} as never,
      '0x' + '11'.repeat(15),
      requestBinding(40, [40]),
    );

    await expect(attempt).rejects.toBeInstanceOf(SummaryAnchorMismatchError);
    await expect(attempt).rejects.toMatchObject({ retryable: true });
    expect(anchor.free).toHaveBeenCalledTimes(1);
  });

  it('frees the anchor when execution itself fails', async () => {
    const anchor = anchorWith('0x' + 'ab'.repeat(32));
    mockChainAnchorForRequest.mockResolvedValue(anchor);
    mockGetSyncHeight.mockResolvedValue(40);
    mockExecuteForSummary.mockRejectedValue(new Error('boom'));

    await expect(
      executeForSummary({} as never, '0x' + '11'.repeat(15), requestBinding(40, [40])),
    ).rejects.toThrow('boom');
    expect(anchor.free).toHaveBeenCalledTimes(1);
  });
});

describe('executeForSummaryAtTip', () => {
  const summary = { marker: 'summary' };

  it('executes without syncing once the client has reached the bound block', async () => {
    mockGetSyncHeight.mockResolvedValue(95);
    mockExecuteForSummary.mockResolvedValue(summary);

    await expect(
      executeForSummaryAtTip({} as never, '0x' + '11'.repeat(15), requestBinding(40, [40])),
    ).resolves.toBe(summary);
    expect(mockSyncState).not.toHaveBeenCalled();
    expect(mockExecuteForSummaryAt).not.toHaveBeenCalled();
  });

  it('syncs once when the client is still below the bound block', async () => {
    mockGetSyncHeight.mockResolvedValueOnce(30).mockResolvedValueOnce(42);
    mockSyncState.mockResolvedValue({});
    mockExecuteForSummary.mockResolvedValue(summary);

    await expect(
      executeForSummaryAtTip({} as never, '0x' + '11'.repeat(15), requestBinding(40, [40])),
    ).resolves.toBe(summary);
    expect(mockSyncState).toHaveBeenCalledTimes(1);
    expect(mockSyncState.mock.invocationCallOrder[0]).toBeLessThan(
      mockExecuteForSummary.mock.invocationCallOrder[0],
    );
  });

  it('refuses to execute when the node has not reached the bound block', async () => {
    mockGetSyncHeight.mockResolvedValue(30);
    mockSyncState.mockResolvedValue({});

    const attempt = executeForSummaryAtTip({} as never, '0x' + '11'.repeat(15), requestBinding(40, [40]));

    await expect(attempt).rejects.toThrow('synced to block 30, below block 40');
    // A lagging node clears on its own, so the failure is worth retrying.
    await expect(attempt).rejects.toBeInstanceOf(ChainBehindBoundBlockError);
    await expect(attempt).rejects.toMatchObject({ retryable: true, syncHeight: 30, boundBlockNum: 40 });
    expect(mockExecuteForSummary).not.toHaveBeenCalled();
  });

  it('refuses a request that binds a block without declaring it', async () => {
    const attempt = executeForSummaryAtTip(
      {} as never,
      '0x' + '11'.repeat(15),
      requestBinding(40, []),
    );

    await expect(attempt).rejects.toBeInstanceOf(BoundBlockNotDeclaredError);
    await expect(attempt).rejects.toMatchObject({ boundBlockNum: 40 });
    expect(mockExecuteForSummary).not.toHaveBeenCalled();
  });

  it('executes a request without multisig auth args as is', async () => {
    mockExecuteForSummary.mockResolvedValue(summary);

    await expect(
      executeForSummaryAtTip({} as never, '0x' + '11'.repeat(15), requestBinding(undefined)),
    ).resolves.toBe(summary);
    expect(mockGetSyncHeight).not.toHaveBeenCalled();
  });
});


describe('syncToBoundBlock', () => {
  const client = () => ({ getSyncHeight: mockGetSyncHeight, syncState: mockSyncState }) as never;

  it('syncs through the caller-supplied sync, so a retry policy can wrap it', async () => {
    mockGetSyncHeight.mockResolvedValueOnce(10).mockResolvedValueOnce(12);
    const retried = vi.fn().mockResolvedValue(undefined);

    await syncToBoundBlock(client(), 12, retried);

    expect(retried).toHaveBeenCalledTimes(1);
    expect(mockSyncState).not.toHaveBeenCalled();
  });

  it('does not sync a client already at or past the bound block', async () => {
    mockGetSyncHeight.mockResolvedValue(12);

    await syncToBoundBlock(client(), 12);

    expect(mockSyncState).not.toHaveBeenCalled();
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
