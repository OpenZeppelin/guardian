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
  summaryApprovalExpirationBlockNum,
  summaryBoundBlockNum,
  summarySalt,
  syncToBoundBlock,
} = await import('./summary.js');
const { BoundBlockNotDeclaredError, TransactionSummaryLayoutError } = await import(
  '../multisig/authArgErrors.js'
);

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

describe('summaryBoundBlockNum', () => {
  /** Block number, block commitment, expiration delta and six user-param felts. */
  const TAIL_BYTES = 4 + 32 + 2 + 48;
  const BLOCK_COMMITMENT = Uint8Array.from({ length: 32 }, (_, i) => i + 1);

  function serializedSummary(blockNum: number, version = 1): Uint8Array {
    const bytes = new Uint8Array(1 + 10 + TAIL_BYTES).fill(0xee);
    const tail = bytes.length - TAIL_BYTES;
    bytes[0] = version;
    new DataView(bytes.buffer).setUint32(tail, blockNum, true);
    bytes.set(BLOCK_COMMITMENT, tail + 4);
    return bytes;
  }

  function summaryOf(bytes: Uint8Array, blockCommitment: Uint8Array = BLOCK_COMMITMENT) {
    const free = vi.fn();
    const summary = {
      serialize: () => bytes,
      blockCommitment: () => ({ serialize: () => blockCommitment, free }),
    } as never;
    return { summary, free };
  }

  function thrownBy(read: () => unknown): unknown {
    try {
      read();
    } catch (error) {
      return error;
    }
    throw new Error('expected the read to throw');
  }

  it('reads the little-endian block number at the start of the tail', () => {
    const { summary, free } = summaryOf(serializedSummary(0x01020304));

    expect(summaryBoundBlockNum(summary)).toBe(0x01020304);
    expect(free).toHaveBeenCalledTimes(1);
  });

  it("refuses a tail whose block commitment is not the summary's", () => {
    const { summary, free } = summaryOf(
      serializedSummary(42),
      BLOCK_COMMITMENT.map((byte) => byte ^ 0xff),
    );

    const error = thrownBy(() => summaryBoundBlockNum(summary));

    expect(error).toBeInstanceOf(TransactionSummaryLayoutError);
    expect(error).toMatchObject({ code: 'transaction_summary_layout_unsupported' });
    expect(free).toHaveBeenCalledTimes(1);
  });

  it('refuses a buffer shorter than the version byte and the tail', () => {
    // Read as if it had a version byte, this tail alone passes both other checks.
    const bytes = new Uint8Array(TAIL_BYTES);
    bytes[0] = 1;
    bytes.set(BLOCK_COMMITMENT, 4);
    const { summary, free } = summaryOf(bytes);

    expect(thrownBy(() => summaryBoundBlockNum(summary))).toBeInstanceOf(
      TransactionSummaryLayoutError,
    );
    expect(free).toHaveBeenCalledTimes(1);
  });

  it('refuses a summary version other than 1', () => {
    const { summary, free } = summaryOf(serializedSummary(42, 2));

    expect(thrownBy(() => summaryBoundBlockNum(summary))).toBeInstanceOf(
      TransactionSummaryLayoutError,
    );
    expect(free).toHaveBeenCalledTimes(1);
  });
});
