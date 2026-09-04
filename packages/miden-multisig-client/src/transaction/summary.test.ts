import { describe, expect, it, vi } from 'vitest';

vi.mock('@miden-sdk/miden-sdk', () => ({
  AccountId: { fromHex: vi.fn() },
  Word: {
    newFromFelts: vi.fn((felts: unknown[]) => ({ felts })),
  },
}));

vi.mock('../raw-client.js', () => ({
  getRawMidenClient: vi.fn(),
}));

const { summaryAuthArg } = await import('./summary.js');

describe('summaryAuthArg', () => {
  it('reads the auth arg from the trailing four user params', () => {
    const summary = {
      userParams: () => [0, 0, 0, 11, 22, 33, 44],
    } as never;

    expect(summaryAuthArg(summary)).toEqual({ felts: [11, 22, 33, 44] });
  });

  it('ignores the leading three user params the auth component zeroes', () => {
    const summary = {
      userParams: () => [7, 8, 9, 11, 22, 33, 44],
    } as never;

    expect(summaryAuthArg(summary)).toEqual({ felts: [11, 22, 33, 44] });
  });
});
