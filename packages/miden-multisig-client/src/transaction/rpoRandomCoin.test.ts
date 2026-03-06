import { describe, expect, it, vi } from 'vitest';
import type { Word } from '@miden-sdk/miden-sdk';

const { mockHashElements, seedFelts } = vi.hoisted(() => {
  const seedFelts = [
    { id: 'seed-0' },
    { id: 'seed-1' },
    { id: 'seed-2' },
    { id: 'seed-3' },
  ];

  return {
    mockHashElements: vi.fn().mockReturnValue({ id: 'drawn-word' }),
    seedFelts,
  };
});

vi.mock('@miden-sdk/miden-sdk', () => {
  class Felt {
    readonly value: bigint;

    constructor(value: bigint) {
      this.value = value;
    }
  }

  class FeltArray {
    readonly values: unknown[];

    constructor(values: unknown[]) {
      this.values = values;
    }
  }

  return {
    Felt,
    FeltArray,
    Rpo256: {
      hashElements: mockHashElements,
    },
  };
});

import { RpoRandomCoin } from './rpoRandomCoin.js';

describe('RpoRandomCoin', () => {
  it('drawWord hashes seed felts plus four zero felts', () => {
    const seed = { toFelts: () => seedFelts } as unknown as Word;
    const coin = new RpoRandomCoin(seed);
    const drawn = coin.drawWord();

    expect(drawn).toEqual({ id: 'drawn-word' });
    expect(mockHashElements).toHaveBeenCalledTimes(1);

    const [feltArrayArg] = mockHashElements.mock.calls[0] as [{ values: unknown[] }];
    const values = feltArrayArg.values;

    expect(values).toHaveLength(8);
    expect(values.slice(0, 4)).toEqual(seedFelts);
    for (const felt of values.slice(4)) {
      expect((felt as { value: bigint }).value).toBe(0n);
    }
  });
});
