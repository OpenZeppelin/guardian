import { describe, it, expect, vi, beforeEach } from 'vitest';
import { AccountInspector } from './inspector.js';

// Mock the Miden SDK
vi.mock('@demox-labs/miden-sdk', () => {
  const createMockWord = (values: bigint[]) => ({
    toU64s: () => values,
    toHex: () => '0x' + values.map(v => v.toString(16).padStart(16, '0')).join(''),
  });

  const createMockStorage = (slots: Map<number, any>, maps: Map<number, Map<string, any>>) => ({
    getItem: (index: number) => slots.get(index) ?? createMockWord([0n, 0n, 0n, 0n]),
    getMapItem: (slotIndex: number, key: any) => {
      const map = maps.get(slotIndex);
      if (!map) throw new Error('Map not found');
      const keyStr = key.toU64s?.()[0]?.toString() ?? '0';
      const value = map.get(keyStr);
      if (!value) throw new Error('Key not found');
      return value;
    },
  });

  const createMockVault = (assets: Array<{ faucetId: string; amount: bigint }>) => ({
    fungibleAssets: () => assets.map(a => ({
      faucetId: () => ({ toString: () => a.faucetId }),
      amount: () => a.amount,
    })),
  });

  return {
    Account: {
      deserialize: vi.fn((bytes: Uint8Array) => {
        // Return different mocked accounts based on test scenario
        // Default: 2-of-3 multisig with PSM enabled
        const slot0 = createMockWord([2n, 3n, 0n, 0n]); // threshold=2, numSigners=3
        const slot4 = createMockWord([1n, 0n, 0n, 0n]); // PSM enabled

        const signerMap = new Map<string, any>();
        signerMap.set('0', createMockWord([BigInt('0x1111111111111111'), BigInt('0x2222222222222222'), BigInt('0x3333333333333333'), BigInt('0x4444444444444444')]));
        signerMap.set('1', createMockWord([BigInt('0x5555555555555555'), BigInt('0x6666666666666666'), BigInt('0x7777777777777777'), BigInt('0x8888888888888888')]));
        signerMap.set('2', createMockWord([BigInt('0xaaaaaaaaaaaaaaaa'), BigInt('0xbbbbbbbbbbbbbbbb'), BigInt('0xcccccccccccccccc'), BigInt('0xdddddddddddddddd')]));

        const psmMap = new Map<string, any>();
        psmMap.set('0', createMockWord([BigInt('0xeeeeeeeeeeeeeeee'), BigInt('0xffffffffffffffff'), BigInt('0x0000000000000001'), BigInt('0x0000000000000002')]));

        const slots = new Map<number, any>();
        slots.set(0, slot0);
        slots.set(4, slot4);

        const maps = new Map<number, Map<string, any>>();
        maps.set(1, signerMap);
        maps.set(5, psmMap);

        return {
          storage: () => createMockStorage(slots, maps),
          vault: () => createMockVault([
            { faucetId: '0xfaucet1', amount: 1000n },
            { faucetId: '0xfaucet2', amount: 500n },
          ]),
        };
      }),
    },
    Word: vi.fn().mockImplementation((arr: BigUint64Array) => ({
      toU64s: () => Array.from(arr),
      toHex: () => '0x' + Array.from(arr).map(v => v.toString(16).padStart(16, '0')).join(''),
    })),
  };
});

describe('AccountInspector', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  describe('fromBase64', () => {
    it('deserializes account from base64 and extracts config', () => {
      const base64 = btoa(String.fromCharCode(...new Uint8Array([1, 2, 3])));
      const config = AccountInspector.fromBase64(base64);

      expect(config.threshold).toBe(2);
      expect(config.numSigners).toBe(3);
    });

    it('extracts PSM status', () => {
      const base64 = btoa(String.fromCharCode(...new Uint8Array([1, 2, 3])));
      const config = AccountInspector.fromBase64(base64);

      expect(config.psmEnabled).toBe(true);
      expect(config.psmCommitment).toMatch(/^0x[a-f0-9]+$/);
    });

    it('extracts signer commitments', () => {
      const base64 = btoa(String.fromCharCode(...new Uint8Array([1, 2, 3])));
      const config = AccountInspector.fromBase64(base64);

      expect(config.signerCommitments).toHaveLength(3);
      config.signerCommitments.forEach(commitment => {
        expect(commitment).toMatch(/^0x[a-f0-9]+$/);
      });
    });

    it('extracts vault balances', () => {
      const base64 = btoa(String.fromCharCode(...new Uint8Array([1, 2, 3])));
      const config = AccountInspector.fromBase64(base64);

      expect(config.vaultBalances).toHaveLength(2);
      expect(config.vaultBalances[0]).toEqual({ faucetId: '0xfaucet1', amount: 1000n });
      expect(config.vaultBalances[1]).toEqual({ faucetId: '0xfaucet2', amount: 500n });
    });
  });

  describe('fromAccount', () => {
    it('extracts threshold from slot 0', async () => {
      const { Account } = await import('@demox-labs/miden-sdk');
      const account = Account.deserialize(new Uint8Array([1, 2, 3]));
      const config = AccountInspector.fromAccount(account);

      expect(config.threshold).toBe(2);
    });

    it('extracts numSigners from slot 0', async () => {
      const { Account } = await import('@demox-labs/miden-sdk');
      const account = Account.deserialize(new Uint8Array([1, 2, 3]));
      const config = AccountInspector.fromAccount(account);

      expect(config.numSigners).toBe(3);
    });
  });
});

describe('AccountInspector edge cases', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('handles account with PSM disabled', async () => {
    const { Account } = await import('@demox-labs/miden-sdk');

    // Override mock for this test
    vi.mocked(Account.deserialize).mockReturnValueOnce({
      storage: () => ({
        getItem: (index: number) => {
          if (index === 0) return { toU64s: () => [1n, 1n, 0n, 0n] };
          if (index === 4) return { toU64s: () => [0n, 0n, 0n, 0n] }; // PSM disabled
          return { toU64s: () => [0n, 0n, 0n, 0n] };
        },
        getMapItem: (slotIndex: number, key: any) => {
          if (slotIndex === 1 && key.toU64s?.()[0] === 0n) {
            return {
              toHex: () => '0x' + 'a'.repeat(64),
              toU64s: () => [1n, 2n, 3n, 4n],
            };
          }
          throw new Error('Not found');
        },
      }),
      vault: () => ({
        fungibleAssets: () => [],
      }),
    } as any);

    const account = Account.deserialize(new Uint8Array([1, 2, 3]));
    const config = AccountInspector.fromAccount(account);

    expect(config.psmEnabled).toBe(false);
    expect(config.psmCommitment).toBeNull();
  });

  it('handles account with empty vault', async () => {
    const { Account } = await import('@demox-labs/miden-sdk');

    vi.mocked(Account.deserialize).mockReturnValueOnce({
      storage: () => ({
        getItem: () => ({ toU64s: () => [1n, 1n, 0n, 0n] }),
        getMapItem: () => {
          throw new Error('Not found');
        },
      }),
      vault: () => ({
        fungibleAssets: () => [],
      }),
    } as any);

    const account = Account.deserialize(new Uint8Array([1, 2, 3]));
    const config = AccountInspector.fromAccount(account);

    expect(config.vaultBalances).toEqual([]);
  });

  it('handles missing signer map entries gracefully', async () => {
    const { Account } = await import('@demox-labs/miden-sdk');

    vi.mocked(Account.deserialize).mockReturnValueOnce({
      storage: () => ({
        getItem: (index: number) => {
          if (index === 0) return { toU64s: () => [2n, 5n, 0n, 0n] }; // threshold=2, numSigners=5
          if (index === 4) return { toU64s: () => [0n, 0n, 0n, 0n] };
          return { toU64s: () => [0n, 0n, 0n, 0n] };
        },
        getMapItem: () => {
          throw new Error('Map entry not found');
        },
      }),
      vault: () => ({
        fungibleAssets: () => [],
      }),
    } as any);

    const account = Account.deserialize(new Uint8Array([1, 2, 3]));
    const config = AccountInspector.fromAccount(account);

    // Should gracefully handle missing entries
    expect(config.numSigners).toBe(5);
    expect(config.signerCommitments).toEqual([]); // All entries missing
  });

  it('handles vault access error gracefully', async () => {
    const { Account } = await import('@demox-labs/miden-sdk');

    vi.mocked(Account.deserialize).mockReturnValueOnce({
      storage: () => ({
        getItem: () => ({ toU64s: () => [1n, 1n, 0n, 0n] }),
        getMapItem: () => {
          throw new Error('Not found');
        },
      }),
      vault: () => {
        throw new Error('Vault access failed');
      },
    } as any);

    const account = Account.deserialize(new Uint8Array([1, 2, 3]));
    const config = AccountInspector.fromAccount(account);

    expect(config.vaultBalances).toEqual([]);
  });
});
