import { describe, it, expect, vi, beforeEach } from 'vitest';
import { AccountInspector } from './inspector.js';

// Storage slot names matching the MASM definitions
const MULTISIG_SLOT_NAMES = {
  THRESHOLD_CONFIG: 'openzeppelin::multisig::threshold_config',
  SIGNER_PUBLIC_KEYS: 'openzeppelin::multisig::signer_public_keys',
  EXECUTED_TRANSACTIONS: 'openzeppelin::multisig::executed_transactions',
  PROCEDURE_THRESHOLDS: 'openzeppelin::multisig::procedure_thresholds',
} as const;

const GUARDIAN_SLOT_NAMES = {
  SELECTOR: 'openzeppelin::guardian::selector',
  PUBLIC_KEY: 'openzeppelin::guardian::public_key',
} as const;

// Mock the Miden SDK
vi.mock('@miden-sdk/miden-sdk', () => {
  const createMockWord = (values: bigint[]) => ({
    toU64s: () => values,
    toHex: () => '0x' + values.map(v => v.toString(16).padStart(16, '0')).join(''),
  });

  const createMockStorage = (slots: Map<string, any>, maps: Map<string, Map<string, any>>) => ({
    getItem: (slotName: string) => slots.get(slotName) ?? createMockWord([0n, 0n, 0n, 0n]),
    getMapItem: (slotName: string, key: any) => {
      const map = maps.get(slotName);
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
        // Default: 2-of-3 multisig with GUARDIAN enabled
        const slot0 = createMockWord([2n, 3n, 0n, 0n]); // threshold=2, numSigners=3
        const slot4 = createMockWord([1n, 0n, 0n, 0n]); // GUARDIAN enabled

        const signerMap = new Map<string, any>();
        signerMap.set('0', createMockWord([BigInt('0x1111111111111111'), BigInt('0x2222222222222222'), BigInt('0x3333333333333333'), BigInt('0x4444444444444444')]));
        signerMap.set('1', createMockWord([BigInt('0x5555555555555555'), BigInt('0x6666666666666666'), BigInt('0x7777777777777777'), BigInt('0x8888888888888888')]));
        signerMap.set('2', createMockWord([BigInt('0xaaaaaaaaaaaaaaaa'), BigInt('0xbbbbbbbbbbbbbbbb'), BigInt('0xcccccccccccccccc'), BigInt('0xdddddddddddddddd')]));

        const guardianMap = new Map<string, any>();
        guardianMap.set('0', createMockWord([BigInt('0xeeeeeeeeeeeeeeee'), BigInt('0xffffffffffffffff'), BigInt('0x0000000000000001'), BigInt('0x0000000000000002')]));

        const slots = new Map<string, any>();
        slots.set('openzeppelin::multisig::threshold_config', slot0);
        slots.set('openzeppelin::guardian::selector', slot4);

        const maps = new Map<string, Map<string, any>>();
        maps.set('openzeppelin::multisig::signer_public_keys', signerMap);
        maps.set('openzeppelin::guardian::public_key', guardianMap);

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

    it('extracts GUARDIAN status', () => {
      const base64 = btoa(String.fromCharCode(...new Uint8Array([1, 2, 3])));
      const config = AccountInspector.fromBase64(base64);

      expect(config.guardianEnabled).toBe(true);
      expect(config.guardianCommitment).toMatch(/^0x[a-f0-9]+$/);
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
      const { Account } = await import('@miden-sdk/miden-sdk');
      const account = Account.deserialize(new Uint8Array([1, 2, 3]));
      const config = AccountInspector.fromAccount(account);

      expect(config.threshold).toBe(2);
    });

    it('extracts numSigners from slot 0', async () => {
      const { Account } = await import('@miden-sdk/miden-sdk');
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

  it('handles account with GUARDIAN disabled', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');

    // Override mock for this test
    vi.mocked(Account.deserialize).mockReturnValueOnce({
      storage: () => ({
        getItem: (slotName: string) => {
          if (slotName === 'openzeppelin::multisig::threshold_config') return { toU64s: () => [1n, 1n, 0n, 0n] };
          if (slotName === 'openzeppelin::guardian::selector') return { toU64s: () => [0n, 0n, 0n, 0n] }; // GUARDIAN disabled
          return { toU64s: () => [0n, 0n, 0n, 0n] };
        },
        getMapItem: (slotName: string, key: any) => {
          if (slotName === 'openzeppelin::multisig::signer_public_keys' && key.toU64s?.()[0] === 0n) {
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

    expect(config.guardianEnabled).toBe(false);
    expect(config.guardianCommitment).toBeNull();
  });

  it('handles account with empty vault', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');

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
    const { Account } = await import('@miden-sdk/miden-sdk');

    vi.mocked(Account.deserialize).mockReturnValueOnce({
      storage: () => ({
        getItem: (slotName: string) => {
          if (slotName === 'openzeppelin::multisig::threshold_config') return { toU64s: () => [2n, 5n, 0n, 0n] }; // threshold=2, numSigners=5
          if (slotName === 'openzeppelin::guardian::selector') return { toU64s: () => [0n, 0n, 0n, 0n] };
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
    const { Account } = await import('@miden-sdk/miden-sdk');

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
