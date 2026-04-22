import { describe, expect, it, vi, beforeEach } from 'vitest';

const { storageMaps, storageSlotMap } = vi.hoisted(() => {
  const storageMaps: Array<{ entries: Array<{ key: unknown; value: unknown }> }> = [];
  const storageSlotMap = vi.fn((name, map) => ({ kind: 'map', name, map }));

  return { storageMaps, storageSlotMap };
});

vi.mock('@miden-sdk/miden-sdk', () => {
  class MockWord {
    data: BigUint64Array | string;

    constructor(data: BigUint64Array | string) {
      this.data = data;
    }

    static fromHex(hex: string) {
      return new MockWord(hex);
    }
  }

  class MockStorageMap {
    entries: Array<{ key: unknown; value: unknown }> = [];

    constructor() {
      storageMaps.push(this);
    }

    insert(key: unknown, value: unknown) {
      this.entries.push({ key, value });
    }
  }

  return {
    Word: MockWord,
    StorageMap: MockStorageMap,
    StorageSlot: {
      fromValue: vi.fn((name, value) => ({ kind: 'value', name, value })),
      map: storageSlotMap,
    },
  };
});

describe('buildMultisigStorageSlots', () => {
  beforeEach(() => {
    storageMaps.length = 0;
    storageSlotMap.mockClear();
  });

  it('keeps single-signer public-key and scheme maps to the configured signer count', async () => {
    const { buildMultisigStorageSlots } = await import('./storage.js');

    buildMultisigStorageSlots({
      threshold: 1,
      signerCommitments: ['0x' + '1'.repeat(64)],
      guardianCommitment: '0x' + '2'.repeat(64),
      signatureScheme: 'falcon',
    });

    const signersMap = storageMaps[0];
    const signerSchemesMap = storageMaps[1];

    expect(signersMap.entries).toHaveLength(1);
    expect(signerSchemesMap.entries).toHaveLength(1);
    expect(signersMap.entries[0].key).toMatchObject({
      data: new BigUint64Array([0n, 0n, 0n, 0n]),
    });
    expect(signerSchemesMap.entries[0].key).toMatchObject({
      data: new BigUint64Array([0n, 0n, 0n, 0n]),
    });
  });

  it('does not pad signer maps when there are already multiple signers', async () => {
    const { buildMultisigStorageSlots } = await import('./storage.js');

    buildMultisigStorageSlots({
      threshold: 1,
      signerCommitments: ['0x' + '1'.repeat(64), '0x' + '3'.repeat(64)],
      guardianCommitment: '0x' + '2'.repeat(64),
      signatureScheme: 'falcon',
    });

    const signersMap = storageMaps[0];
    const signerSchemesMap = storageMaps[1];

    expect(signersMap.entries).toHaveLength(2);
    expect(signerSchemesMap.entries).toHaveLength(2);
  });
});
