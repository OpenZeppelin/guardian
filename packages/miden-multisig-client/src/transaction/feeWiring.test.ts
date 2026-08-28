import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { Note, Word } from '@miden-sdk/miden-sdk';

/**
 * Pins the fee-auth wiring in the four builders that share it, which nothing else
 * reaches: `src/multisig.test.ts` replaces this whole module with a mock, so their
 * bodies never run there, and `p2id` is covered in `p2id.test.ts`.
 *
 * Both directions matter. Losing `withAuthArg(feeAuth.authArg)` would send a bare
 * salt where the cosigners signed a commitment, so the rebuild at execute time
 * would fail its reconstruction check after signatures were collected. Losing
 * `extendAdviceMap(feeAuth.adviceMap)` would keep the commitment but drop its
 * preimage, which is exactly the on-chain abort this module exists to prevent.
 */

const { mockWithAuthArg, mockExtendAdviceMap, mockCompileTxScript } = vi.hoisted(() => ({
  mockWithAuthArg: vi.fn(),
  mockExtendAdviceMap: vi.fn(),
  mockCompileTxScript: vi.fn().mockResolvedValue({ kind: 'script' }),
}));

vi.mock('@miden-sdk/miden-sdk', () => {
  class Felt {
    constructor(readonly value: bigint) {}
  }

  class FeltArray {
    constructor(readonly values: unknown[]) {}
  }

  class AdviceMap {
    readonly entries: Array<[unknown, unknown]> = [];

    insert(key: unknown, value: unknown): void {
      this.entries.push([key, value]);
    }
  }

  class NoteAndArgs {
    constructor(_note: unknown, _args: unknown) {}
  }

  class NoteAndArgsArray {
    push(_entry: unknown): void {}
  }

  class TransactionRequestBuilder {
    withCustomScript(_script: unknown): this {
      return this;
    }

    withScriptArg(_arg: unknown): this {
      return this;
    }

    withInputNotes(_notes: unknown): this {
      return this;
    }

    withAuthArg(authArg: unknown): this {
      mockWithAuthArg(authArg);
      return this;
    }

    extendAdviceMap(adviceMap: unknown): this {
      mockExtendAdviceMap(adviceMap);
      return this;
    }

    build(): { kind: 'request' } {
      return { kind: 'request' };
    }
  }

  const word = (hex: string) => ({
    toHex: () => hex,
    toFelts: () => [{ id: `${hex}-0` }, { id: `${hex}-1` }, { id: `${hex}-2` }, { id: `${hex}-3` }],
    free: () => {},
  });

  return {
    AccountId: {
      fromHex: vi.fn((hex: string) => ({
        prefix: () => new Felt(1n),
        suffix: () => new Felt(2n),
        toString: () => hex,
        free: () => {},
      })),
    },
    AdviceMap,
    Felt,
    FeltArray,
    NoteAndArgs,
    NoteAndArgsArray,
    Poseidon2: {
      // The fee commitment is the only hash over exactly eight felts here, so
      // keying on that keeps it distinguishable from a builder's config hash.
      hashElements: vi.fn((elements: FeltArray) =>
        word(elements.values.length === 8 ? '0xcommitment' : '0xconfighash'),
      ),
    },
    TransactionRequestBuilder,
    Word: {
      fromHex: vi.fn((hex: string) => word(hex)),
      newFromFelts: vi.fn(() => word('0xconversioninfo')),
    },
  };
});

vi.mock('../raw-client.js', () => ({
  compileTxScript: mockCompileTxScript,
}));

import { buildConsumeNotesTransactionRequestFromNotes } from './consumeNotes.js';
import { buildUpdateGuardianTransactionRequest } from './updateGuardian.js';
import { buildUpdateProcedureThresholdTransactionRequest } from './updateProcedureThreshold.js';
import { buildUpdateSignersTransactionRequest } from './updateSigners.js';

const SALT = { toHex: () => '0x' + '11'.repeat(32) } as unknown as Word;
const FEE_FAUCET = '0xade67f7701e9e9c12493c6206bc46e';
// What the mocked Poseidon2 returns, so the commitment is identifiable by hex
// rather than merely "not the salt".
const COMMITMENT = '0xcommitment';
const CONVERSION_INFO = '0xconversioninfo';
const feltIds = (hex: string) => [`${hex}-0`, `${hex}-1`, `${hex}-2`, `${hex}-3`];
const GUARDIAN_PUBKEY = '0x' + 'ab'.repeat(32);
const SIGNER_COMMITMENT = '0x' + 'cd'.repeat(32);
const client = {} as never;

// Each entry drives one builder to its `withAuthArg` call. `baseAdviceMaps` is how
// many advice maps the builder extends for reasons unrelated to fees, so the fee
// entry shows up as exactly one more.
const builders: Array<{
  name: string;
  baseAdviceMaps: number;
  build: (feeFaucetId?: string) => Promise<unknown>;
}> = [
  {
    name: 'buildUpdateSignersTransactionRequest',
    baseAdviceMaps: 1,
    build: (feeFaucetId) =>
      buildUpdateSignersTransactionRequest(client, 2, [SIGNER_COMMITMENT], {
        salt: SALT,
        feeFaucetId,
      }),
  },
  {
    name: 'buildUpdateProcedureThresholdTransactionRequest',
    baseAdviceMaps: 0,
    build: (feeFaucetId) =>
      buildUpdateProcedureThresholdTransactionRequest(client, 'update_signers', 2, {
        salt: SALT,
        feeFaucetId,
      }),
  },
  {
    name: 'buildUpdateGuardianTransactionRequest',
    baseAdviceMaps: 0,
    build: (feeFaucetId) =>
      buildUpdateGuardianTransactionRequest(client, GUARDIAN_PUBKEY, {
        salt: SALT,
        feeFaucetId,
      }),
  },
  {
    name: 'buildConsumeNotesTransactionRequestFromNotes',
    baseAdviceMaps: 0,
    build: async (feeFaucetId) =>
      buildConsumeNotesTransactionRequestFromNotes([{} as Note], {
        salt: SALT,
        feeFaucetId,
      }),
  },
];

describe('fee-auth wiring across the shared builders', () => {
  beforeEach(() => {
    mockWithAuthArg.mockClear();
    mockExtendAdviceMap.mockClear();
  });

  for (const { name, baseAdviceMaps, build } of builders) {
    it(`${name} passes the bare salt and adds no fee advice by default`, async () => {
      await build(undefined);

      expect(mockWithAuthArg).toHaveBeenCalledTimes(1);
      const [authArg] = mockWithAuthArg.mock.calls[0] as [{ toHex: () => string }];
      expect(authArg.toHex()).toBe(SALT.toHex());
      expect(mockExtendAdviceMap).toHaveBeenCalledTimes(baseAdviceMaps);
    });

    it(`${name} commits to conversion info and adds its preimage with a fee faucet`, async () => {
      await build(FEE_FAUCET);

      expect(mockWithAuthArg).toHaveBeenCalledTimes(1);
      const [authArg] = mockWithAuthArg.mock.calls[0] as [{ toHex: () => string }];
      expect(authArg.toHex()).toBe(COMMITMENT);
      expect(mockExtendAdviceMap).toHaveBeenCalledTimes(baseAdviceMaps + 1);

      // The preimage has to reach the builder under the commitment as its key, in
      // SALT ++ CONVERSION_INFO order. Asserting the call count alone would accept
      // any object at all.
      const feeEntries = mockExtendAdviceMap.mock.calls
        .flatMap(([adviceMap]) => (adviceMap as { entries?: Array<[unknown, unknown]> }).entries ?? [])
        .filter(([key]) => (key as { toHex: () => string }).toHex() === COMMITMENT);

      expect(feeEntries).toHaveLength(1);
      const [, preimage] = feeEntries[0] as [unknown, { values: Array<{ id: string }> }];
      expect(preimage.values.map((felt) => felt.id)).toEqual([
        ...feltIds(SALT.toHex()),
        ...feltIds(CONVERSION_INFO),
      ]);
    });
  }
});
