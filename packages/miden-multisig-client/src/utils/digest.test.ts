import { describe, expect, it, vi, beforeEach } from 'vitest';
import { AuthDigest } from './digest.js';

vi.mock('@miden-sdk/miden-sdk', () => ({
  AccountId: {
    fromHex: vi.fn(() => ({
      prefix: () => ({ label: 'prefix' }),
      suffix: () => ({ label: 'suffix' }),
    })),
  },
  Felt: vi.fn().mockImplementation((value: bigint) => ({ value })),
  FeltArray: vi.fn().mockImplementation((elements: unknown[]) => elements),
  Word: {
    fromHex: vi.fn((hex: string) => ({
      toHex: () => hex,
      toFelts: () => [],
    })),
  },
  Rpo256: {
    hashElements: vi.fn().mockReturnValue({
      toHex: () => '0x' + 'aa'.repeat(32),
      toFelts: () => [],
    }),
  },
}));

describe('AuthDigest', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('uses the legacy 4-felt digest for account auth', async () => {
    const { FeltArray, Rpo256 } = await import('@miden-sdk/miden-sdk');

    AuthDigest.fromAccountIdWithTimestamp('0x' + 'ab'.repeat(15), 1700000000);

    expect(FeltArray).toHaveBeenCalledWith([
      { label: 'prefix' },
      { label: 'suffix' },
      { value: 1700000000n },
      { value: 0n },
    ]);
    expect(Rpo256.hashElements).toHaveBeenCalledTimes(1);
  });

  it('includes the request payload hash for request-bound auth', async () => {
    const { Rpo256 } = await import('@miden-sdk/miden-sdk');

    AuthDigest.fromRequest('0x' + 'ab'.repeat(15), 1700000000, {
      toBytes: () => new Uint8Array([1, 2, 3, 4]),
    } as never);

    expect(Rpo256.hashElements).toHaveBeenCalledTimes(2);
  });
});
