import { describe, it, expect, vi, beforeEach } from 'vitest';
import { AccountInspector, assertCompleteDetectedConfig } from './inspector.js';
import type { DetectedMultisigConfig } from './inspector.js';

// Mock the Miden SDK
vi.mock('@miden-sdk/miden-sdk', () => {
  const createMockWord = (values: bigint[]) => ({
    toU64s: () => values,
    toHex: () => '0x' + values.map(v => v.toString(16).padStart(16, '0')).join(''),
  });

  // Faithful to the real SDK's read semantics (verified against the wasm
  // glue and miden-protocol): getItem returns undefined for an absent slot;
  // getMapItem returns undefined only when the slot is absent or not a map,
  // and the EMPTY word for a missing key in an existing map
  // (StorageMap::get is unwrap_or_default). Neither throws for "not found".
  const createMockStorage = (slots: Map<string, any>, maps: Map<string, Map<string, any>>) => ({
    getItem: (slotName: string) => slots.get(slotName),
    getMapItem: (slotName: string, key: any) => {
      const map = maps.get(slotName);
      if (!map) return undefined;
      const keyStr = key.toU64s?.()[0]?.toString() ?? '0';
      return map.get(keyStr) ?? createMockWord([0n, 0n, 0n, 0n]);
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

        const signerMap = new Map<string, any>();
        signerMap.set('0', createMockWord([BigInt('0x1111111111111111'), BigInt('0x2222222222222222'), BigInt('0x3333333333333333'), BigInt('0x4444444444444444')]));
        signerMap.set('1', createMockWord([BigInt('0x5555555555555555'), BigInt('0x6666666666666666'), BigInt('0x7777777777777777'), BigInt('0x8888888888888888')]));
        signerMap.set('2', createMockWord([BigInt('0xaaaaaaaaaaaaaaaa'), BigInt('0xbbbbbbbbbbbbbbbb'), BigInt('0xcccccccccccccccc'), BigInt('0xdddddddddddddddd')]));

        const guardianMap = new Map<string, any>();
        guardianMap.set('0', createMockWord([BigInt('0xeeeeeeeeeeeeeeee'), BigInt('0xffffffffffffffff'), BigInt('0x0000000000000001'), BigInt('0x0000000000000002')]));

        const slots = new Map<string, any>();
        slots.set('miden::standards::auth::multisig::threshold_config', slot0);

        const maps = new Map<string, Map<string, any>>();
        maps.set('miden::standards::auth::multisig::approver_public_keys', signerMap);
        maps.set('miden::standards::auth::guardian::pub_key', guardianMap);

        return {
          storage: () => createMockStorage(slots, maps),
          // The contract-version guard checks for the pinned auth procedure.
          code: () => ({ hasProcedure: () => true }),
          vault: () => createMockVault([
            { faucetId: '0xfaucet1', amount: 1000n },
            { faucetId: '0xfaucet2', amount: 500n },
          ]),
        };
      }),
    },
    Word: Object.assign(
      vi.fn().mockImplementation((arr: BigUint64Array) => ({
        toU64s: () => Array.from(arr),
        toHex: () => '0x' + Array.from(arr).map(v => v.toString(16).padStart(16, '0')).join(''),
      })),
      {
        fromHex: vi.fn((hex: string) => ({
          toU64s: () => [0n, 0n, 0n, 0n],
          toHex: () => hex,
        })),
      },
    ),
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

    it('extracts the GUARDIAN commitment', () => {
      const base64 = btoa(String.fromCharCode(...new Uint8Array([1, 2, 3])));
      const config = AccountInspector.fromBase64(base64);

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

    it('rejects accounts built from a different contract version', async () => {
      const { Account } = await import('@miden-sdk/miden-sdk');
      const account = Account.deserialize(new Uint8Array([1, 2, 3]));
      // Same account shape, but its code lacks the pinned auth procedure —
      // root-keyed reads against it would silently miss its stored overrides.
      const foreign = {
        ...account,
        code: () => ({ hasProcedure: () => false }),
      };

      expect(() => AccountInspector.fromAccount(foreign as never)).toThrow(
        /unsupported contract version/,
      );
    });
  });
});

describe('AccountInspector edge cases', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('returns a null GUARDIAN commitment when the guardian pub_key slot is absent', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');

    // Override mock for this test: no guardian pub_key entry present.
    vi.mocked(Account.deserialize).mockReturnValueOnce({
      code: () => ({ hasProcedure: () => true }),
      storage: () => ({
        getItem: (slotName: string) => {
          if (slotName === 'miden::standards::auth::multisig::threshold_config') return { toU64s: () => [1n, 1n, 0n, 0n] };
          return { toU64s: () => [0n, 0n, 0n, 0n] };
        },
        getMapItem: (slotName: string, key: any) => {
          if (slotName === 'miden::standards::auth::multisig::approver_public_keys' && key.toU64s?.()[0] === 0n) {
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

    expect(config.guardianCommitment).toBeNull();
  });

  it('handles account with empty vault', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');

    vi.mocked(Account.deserialize).mockReturnValueOnce({
      code: () => ({ hasProcedure: () => true }),
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
      code: () => ({ hasProcedure: () => true }),
      storage: () => ({
        getItem: (slotName: string) => {
          if (slotName === 'miden::standards::auth::multisig::threshold_config') return { toU64s: () => [2n, 5n, 0n, 0n] }; // threshold=2, numSigners=5
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
      code: () => ({ hasProcedure: () => true }),
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

describe('AccountInspector.getSignerPublicKeyCommitments', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('returns all signer commitments ordered by signer index', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');
    const account = Account.deserialize(new Uint8Array([1, 2, 3]));

    const commitments = AccountInspector.getSignerPublicKeyCommitments(account);

    expect(commitments).toEqual([
      '0x1111111111111111222222222222222233333333333333334444444444444444',
      '0x5555555555555555666666666666666677777777777777778888888888888888',
      '0xaaaaaaaaaaaaaaaabbbbbbbbbbbbbbbbccccccccccccccccdddddddddddddddd',
    ]);
  });

  it('rejects accounts built from a different contract version', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');
    const account = Account.deserialize(new Uint8Array([1, 2, 3]));
    const foreign = {
      ...account,
      code: () => ({ hasProcedure: () => false }),
    };

    expect(() => AccountInspector.getSignerPublicKeyCommitments(foreign as never)).toThrow(
      /unsupported contract version/,
    );
  });

  it('throws when a signer map entry is the empty word instead of truncating', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');

    // Real SDK semantics: entries 0 and 1 exist, entries 2..4 come back as
    // the empty word (StorageMap::get is unwrap_or_default) — NOT undefined
    // and NOT a throw.
    const entries = new Map<string, any>([
      ['0', { toU64s: () => [1n, 2n, 3n, 4n], toHex: () => '0x' + 'a'.repeat(64) }],
      ['1', { toU64s: () => [5n, 6n, 7n, 8n], toHex: () => '0x' + 'b'.repeat(64) }],
    ]);
    vi.mocked(Account.deserialize).mockReturnValueOnce({
      code: () => ({ hasProcedure: () => true }),
      storage: () => ({
        getItem: (slotName: string) => {
          if (slotName === 'miden::standards::auth::multisig::threshold_config') return { toU64s: () => [2n, 5n, 0n, 0n] };
          return undefined;
        },
        getMapItem: (slotName: string, key: any) => {
          if (slotName !== 'miden::standards::auth::multisig::approver_public_keys') return undefined;
          const keyStr = key.toU64s?.()[0]?.toString() ?? '0';
          return entries.get(keyStr) ?? { toU64s: () => [0n, 0n, 0n, 0n] };
        },
      }),
    } as any);

    const account = Account.deserialize(new Uint8Array([1, 2, 3]));

    expect(() => AccountInspector.getSignerPublicKeyCommitments(account)).toThrow(
      /missing signer public key at index 2/
    );
  });

  it('throws when the signer map slot itself is absent', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');

    vi.mocked(Account.deserialize).mockReturnValueOnce({
      code: () => ({ hasProcedure: () => true }),
      storage: () => ({
        getItem: (slotName: string) => {
          if (slotName === 'miden::standards::auth::multisig::threshold_config') return { toU64s: () => [2n, 3n, 0n, 0n] };
          return undefined;
        },
        getMapItem: () => undefined,
      }),
    } as any);

    const account = Account.deserialize(new Uint8Array([1, 2, 3]));

    expect(() => AccountInspector.getSignerPublicKeyCommitments(account)).toThrow(
      /missing signer public key at index 0/
    );
  });

  it('throws when the threshold config slot is absent', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');

    vi.mocked(Account.deserialize).mockReturnValueOnce({
      code: () => ({ hasProcedure: () => true }),
      storage: () => ({
        getItem: () => undefined,
        getMapItem: () => undefined,
      }),
    } as any);

    const account = Account.deserialize(new Uint8Array([1, 2, 3]));

    expect(() => AccountInspector.getSignerPublicKeyCommitments(account)).toThrow(
      /not a guarded-multisig account/
    );
  });

  it('throws when the threshold config reports zero signers', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');

    vi.mocked(Account.deserialize).mockReturnValueOnce({
      code: () => ({ hasProcedure: () => true }),
      storage: () => ({
        getItem: () => ({ toU64s: () => [0n, 0n, 0n, 0n] }),
        getMapItem: () => undefined,
      }),
    } as any);

    const account = Account.deserialize(new Uint8Array([1, 2, 3]));

    expect(() => AccountInspector.getSignerPublicKeyCommitments(account)).toThrow(
      /zero signers/
    );
  });

  // 2n ** 53n is the smallest count whose Number() conversion is no longer a
  // safe integer (i++ would stop advancing without the ceiling guard).
  it.each([4294967295n, 2n ** 53n])(
    'throws on an absurd signer count (%s) instead of looping',
    async (count) => {
      const { Account } = await import('@miden-sdk/miden-sdk');

      vi.mocked(Account.deserialize).mockReturnValueOnce({
        code: () => ({ hasProcedure: () => true }),
        storage: () => ({
          getItem: () => ({ toU64s: () => [2n, count, 0n, 0n] }),
          getMapItem: () => ({ toU64s: () => [1n, 2n, 3n, 4n], toHex: () => '0x' + 'a'.repeat(64) }),
        }),
      } as any);

      const account = Account.deserialize(new Uint8Array([1, 2, 3]));

      expect(() => AccountInspector.getSignerPublicKeyCommitments(account)).toThrow(
        /sanity limit/
      );
    },
  );

  it('surfaces a descriptive error for an account from a different SDK copy', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');

    // wasm-bindgen's _assertClass rejects Word instances from another
    // bundled SDK copy with this exact message.
    vi.mocked(Account.deserialize).mockReturnValueOnce({
      code: () => ({ hasProcedure: () => true }),
      storage: () => ({
        getItem: (slotName: string) => {
          if (slotName === 'miden::standards::auth::multisig::threshold_config') return { toU64s: () => [1n, 1n, 0n, 0n] };
          return undefined;
        },
        getMapItem: () => {
          throw new Error('expected instance of Word');
        },
      }),
    } as any);

    const account = Account.deserialize(new Uint8Array([1, 2, 3]));

    expect(() => AccountInspector.getSignerPublicKeyCommitments(account)).toThrow(
      /different copy of @miden-sdk\/miden-sdk/
    );
  });
});

describe('AccountInspector.getGuardianPublicKeyCommitment', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('returns the guardian commitment', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');
    const account = Account.deserialize(new Uint8Array([1, 2, 3]));

    const commitment = AccountInspector.getGuardianPublicKeyCommitment(account);

    expect(commitment).toBe('0xeeeeeeeeeeeeeeeeffffffffffffffff00000000000000010000000000000002');
  });

  it('rejects accounts built from a different contract version', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');
    const account = Account.deserialize(new Uint8Array([1, 2, 3]));
    const foreign = {
      ...account,
      code: () => ({ hasProcedure: () => false }),
    };

    expect(() => AccountInspector.getGuardianPublicKeyCommitment(foreign as never)).toThrow(
      /unsupported contract version/,
    );
  });

  it('throws when the guardian key entry is the empty word', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');

    vi.mocked(Account.deserialize).mockReturnValueOnce({
      code: () => ({ hasProcedure: () => true }),
      storage: () => ({
        getItem: () => ({ toU64s: () => [1n, 1n, 0n, 0n] }),
        getMapItem: () => ({ toU64s: () => [0n, 0n, 0n, 0n] }),
      }),
    } as any);

    const account = Account.deserialize(new Uint8Array([1, 2, 3]));

    expect(() => AccountInspector.getGuardianPublicKeyCommitment(account)).toThrow(
      /inconsistent account state/
    );
  });

  it('propagates genuine storage read failures', async () => {
    const { Account } = await import('@miden-sdk/miden-sdk');

    vi.mocked(Account.deserialize).mockReturnValueOnce({
      code: () => ({ hasProcedure: () => true }),
      storage: () => ({
        getItem: () => ({ toU64s: () => [1n, 1n, 0n, 0n] }),
        getMapItem: () => {
          throw new Error('storage backend exploded');
        },
      }),
    } as any);

    const account = Account.deserialize(new Uint8Array([1, 2, 3]));

    expect(() => AccountInspector.getGuardianPublicKeyCommitment(account)).toThrow(
      'storage backend exploded'
    );
  });
});

describe('assertCompleteDetectedConfig', () => {
  const complete: DetectedMultisigConfig = {
    threshold: 2,
    numSigners: 2,
    signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
    guardianCommitment: '0x' + 'c'.repeat(64),
    vaultBalances: [],
    procedureThresholds: new Map(),
  };

  it('accepts a complete config', () => {
    expect(() => assertCompleteDetectedConfig(complete)).not.toThrow();
  });

  it('rejects a signer set shorter than the reported count', () => {
    expect(() =>
      assertCompleteDetectedConfig({
        ...complete,
        numSigners: 3,
      }),
    ).toThrow(/incomplete signer set: storage reports 3 signers, read 2/);
  });

  it('rejects a zero signer count', () => {
    expect(() =>
      assertCompleteDetectedConfig({
        ...complete,
        numSigners: 0,
        signerCommitments: [],
      }),
    ).toThrow(/incomplete signer set/);
  });

  it('rejects a missing guardian commitment', () => {
    expect(() =>
      assertCompleteDetectedConfig({
        ...complete,
        guardianCommitment: null,
      }),
    ).toThrow(/missing guardian commitment/);
  });
});
