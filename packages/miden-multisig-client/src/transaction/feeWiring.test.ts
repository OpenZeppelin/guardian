import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { Note, Word } from '@miden-sdk/miden-sdk';

const { mockWithFeeConversionSalt, mockCompileTxScript } = vi.hoisted(() => ({
  mockWithFeeConversionSalt: vi.fn(),
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
    insert(_key: unknown, _value: unknown): void {}
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

    withFeeConversionSalt(salt: unknown): this {
      mockWithFeeConversionSalt(salt);
      return this;
    }

    extendAdviceMap(_adviceMap: unknown): this {
      return this;
    }

    build(): { kind: 'request' } {
      return { kind: 'request' };
    }
  }

  const word = (hex: string) => ({
    toHex: () => hex,
    toFelts: () => [],
  });

  return {
    AdviceMap,
    Felt,
    FeltArray,
    NoteAndArgs,
    NoteAndArgsArray,
    Poseidon2: {
      hashElements: vi.fn(() => word('0xconfighash')),
    },
    TransactionRequestBuilder,
    Word: {
      fromHex: vi.fn((hex: string) => word(hex)),
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
const GUARDIAN_PUBKEY = '0x' + 'ab'.repeat(32);
const SIGNER_COMMITMENT = '0x' + 'cd'.repeat(32);
const client = {} as never;

const builders: Array<{
  name: string;
  build: () => Promise<unknown>;
}> = [
  {
    name: 'buildUpdateSignersTransactionRequest',
    build: () =>
      buildUpdateSignersTransactionRequest(client, 2, [SIGNER_COMMITMENT], {
        salt: SALT,
      }),
  },
  {
    name: 'buildUpdateProcedureThresholdTransactionRequest',
    build: () =>
      buildUpdateProcedureThresholdTransactionRequest(client, 'update_signers', 2, {
        salt: SALT,
      }),
  },
  {
    name: 'buildUpdateGuardianTransactionRequest',
    build: () =>
      buildUpdateGuardianTransactionRequest(client, GUARDIAN_PUBKEY, {
        salt: SALT,
      }),
  },
  {
    name: 'buildConsumeNotesTransactionRequestFromNotes',
    build: async () =>
      buildConsumeNotesTransactionRequestFromNotes([{} as Note], {
        salt: SALT,
      }),
  },
];

describe('fee conversion salt wiring across transaction builders', () => {
  beforeEach(() => {
    mockWithFeeConversionSalt.mockClear();
  });

  for (const { name, build } of builders) {
    it(`${name} declares the proposal salt for fee conversion`, async () => {
      await build();

      expect(mockWithFeeConversionSalt).toHaveBeenCalledTimes(1);
      const [salt] = mockWithFeeConversionSalt.mock.calls[0] as [{ toHex: () => string }];
      expect(salt.toHex()).toBe(SALT.toHex());
    });
  }
});
