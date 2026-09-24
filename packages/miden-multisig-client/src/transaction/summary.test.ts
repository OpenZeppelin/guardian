import { describe, expect, it, vi } from 'vitest';

const { mockChainAnchorForRequest, mockExecuteForSummaryAt } = vi.hoisted(() => ({
  mockChainAnchorForRequest: vi.fn(),
  mockExecuteForSummaryAt: vi.fn(),
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
    executeForSummaryAt: mockExecuteForSummaryAt,
  })),
}));

const { executeForSummary, summaryApprovalExpirationBlockNum, summarySalt, SummaryAnchorMismatchError } =
  await import('./summary.js');

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

  it('returns the anchor with a summary that binds the anchor block', async () => {
    const anchor = anchorWith('0x' + 'ab'.repeat(32));
    mockChainAnchorForRequest.mockResolvedValue(anchor);
    mockExecuteForSummaryAt.mockResolvedValue(summaryBinding('0x' + 'AB'.repeat(32)));

    const result = await executeForSummary({} as never, '0x' + '11'.repeat(15), {} as never);

    expect(result.anchor).toBe(anchor);
    expect(anchor.free).not.toHaveBeenCalled();
  });

  it('frees the anchor and fails when the summary binds another block', async () => {
    const anchor = anchorWith('0x' + 'ab'.repeat(32));
    mockChainAnchorForRequest.mockResolvedValue(anchor);
    mockExecuteForSummaryAt.mockResolvedValue(summaryBinding('0x' + 'cd'.repeat(32)));

    const attempt = executeForSummary({} as never, '0x' + '11'.repeat(15), {} as never);

    await expect(attempt).rejects.toBeInstanceOf(SummaryAnchorMismatchError);
    await expect(attempt).rejects.toMatchObject({ retryable: true });
    expect(anchor.free).toHaveBeenCalledTimes(1);
  });

  it('frees the anchor when execution itself fails', async () => {
    const anchor = anchorWith('0x' + 'ab'.repeat(32));
    mockChainAnchorForRequest.mockResolvedValue(anchor);
    mockExecuteForSummaryAt.mockRejectedValue(new Error('boom'));

    await expect(
      executeForSummary({} as never, '0x' + '11'.repeat(15), {} as never),
    ).rejects.toThrow('boom');
    expect(anchor.free).toHaveBeenCalledTimes(1);
  });
});

