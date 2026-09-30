import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { Note, Word } from '@miden-sdk/miden-sdk';

const { mockFeeAwareBuilder, mockCompileTxScript, builderCalls } = vi.hoisted(() => ({
  mockFeeAwareBuilder: vi.fn(),
  mockCompileTxScript: vi.fn().mockResolvedValue({ kind: 'script' }),
  builderCalls: [] as string[],
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
    constructor(readonly authArg: unknown) {}

    withCustomScript(_script: unknown): this {
      builderCalls.push('withCustomScript');
      return this;
    }

    withScriptArg(_arg: unknown): this {
      builderCalls.push('withScriptArg');
      return this;
    }

    withInputNotes(_notes: unknown): this {
      builderCalls.push('withInputNotes');
      return this;
    }

    withFeeConversionSalt(_salt: unknown): this {
      builderCalls.push('withFeeConversionSalt');
      return this;
    }

    withAuthArg(_authArg: unknown): this {
      builderCalls.push('withAuthArg');
      return this;
    }

    extendAdviceMap(_adviceMap: unknown): this {
      builderCalls.push('extendAdviceMap');
      return this;
    }

    build(): { kind: 'request'; authArg: () => unknown } {
      return { kind: 'request', authArg: () => this.authArg };
    }
  }

  const word = (hex: string) => ({
    toHex: () => hex,
    toFelts: () => [],
  });

  return {
    AccountId: { fromHex: vi.fn((hex: string) => ({ hex })) },
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

vi.mock('../raw-client.js', async () => {
  const actual = await vi.importActual<typeof import('../raw-client.js')>('../raw-client.js');
  return {
    ...actual,
    compileTxScript: mockCompileTxScript,
    getRawMidenClient: vi.fn(async (client: unknown) => client),
  };
});

const { MultisigAuthArgsMissingError } = await import('../multisig/authArgErrors.js');
const { buildConsumeNotesTransactionRequestFromNotes } = await import('./consumeNotes.js');
const { buildUpdateGuardianTransactionRequest } = await import('./updateGuardian.js');
const { buildUpdateProcedureThresholdTransactionRequest } = await import(
  './updateProcedureThreshold.js'
);
const { buildUpdateSignersTransactionRequest } = await import('./updateSigners.js');
const { TransactionRequestBuilder } = await import('@miden-sdk/miden-sdk');
const FakeBuilder = TransactionRequestBuilder as unknown as new (
  authArg: unknown,
) => InstanceType<typeof TransactionRequestBuilder>;

const SALT = { toHex: () => '0x' + '11'.repeat(32) } as unknown as Word;
const GUARDIAN_PUBKEY = '0x' + 'ab'.repeat(32);
const SIGNER_COMMITMENT = '0x' + 'cd'.repeat(32);
const ACCOUNT_ID = '0x' + '7b'.repeat(15);
const BOUND_BLOCK = 4242;

const client = { feeAwareTransactionRequestBuilder: mockFeeAwareBuilder } as never;

const builders: Array<{
  name: string;
  build: (options: {
    accountId: string;
    salt: Word;
    boundBlockNum?: number;
    approvalExpirationDelta?: number;
  }) => Promise<unknown>;
}> = [
  {
    name: 'buildUpdateSignersTransactionRequest',
    build: (options) =>
      buildUpdateSignersTransactionRequest(client, 2, [SIGNER_COMMITMENT], options),
  },
  {
    name: 'buildUpdateProcedureThresholdTransactionRequest',
    build: (options) =>
      buildUpdateProcedureThresholdTransactionRequest(client, 'update_signers', 2, options),
  },
  {
    name: 'buildUpdateGuardianTransactionRequest',
    build: (options) => buildUpdateGuardianTransactionRequest(client, GUARDIAN_PUBKEY, options),
  },
  {
    name: 'buildConsumeNotesTransactionRequestFromNotes',
    build: (options) =>
      buildConsumeNotesTransactionRequestFromNotes(client, [{} as Note], options),
  },
];

describe('multisig auth args wiring across transaction builders', () => {
  beforeEach(() => {
    builderCalls.length = 0;
    mockFeeAwareBuilder.mockReset();
    mockFeeAwareBuilder.mockImplementation(async () => new FakeBuilder({ set: true }));
  });

  for (const { name, build } of builders) {
    it(`${name} asks the client for a builder carrying the account's auth args`, async () => {
      await build({ accountId: ACCOUNT_ID, salt: SALT });

      expect(mockFeeAwareBuilder).toHaveBeenCalledTimes(1);
      const [accountId, expiration, salt, boundBlockNum] = mockFeeAwareBuilder.mock.calls[0] as [
        { hex: string },
        unknown,
        { toHex: () => string },
        unknown,
      ];
      expect(accountId.hex).toBe(ACCOUNT_ID);
      expect(expiration).toBeNull();
      expect(salt.toHex()).toBe(SALT.toHex());
      expect(boundBlockNum).toBeNull();
    });

    it(`${name} pins the bound block a rebuild names`, async () => {
      await build({ accountId: ACCOUNT_ID, salt: SALT, boundBlockNum: BOUND_BLOCK });

      expect(mockFeeAwareBuilder.mock.calls[0]?.[3]).toBe(BOUND_BLOCK);
    });

    it(`${name} passes the approval expiration the proposer asked for`, async () => {
      await build({ accountId: ACCOUNT_ID, salt: SALT, approvalExpirationDelta: 100 });

      expect(mockFeeAwareBuilder.mock.calls[0]?.[1]).toBe(100);
    });

    it(`${name} refuses a zero approval expiration instead of letting the kernel reject it`, async () => {
      await expect(
        build({ accountId: ACCOUNT_ID, salt: SALT, approvalExpirationDelta: 0 }),
      ).rejects.toThrow(/approvalExpirationDelta must be a whole number of blocks/);
      expect(mockFeeAwareBuilder).not.toHaveBeenCalled();
    });

    it(`${name} never touches the setters that would clear the auth args`, async () => {
      await build({ accountId: ACCOUNT_ID, salt: SALT });

      expect(builderCalls).not.toContain('withFeeConversionSalt');
      expect(builderCalls).not.toContain('withAuthArg');
    });

    it(`${name} refuses a request the client handed back without auth args`, async () => {
      mockFeeAwareBuilder.mockImplementation(async () => new FakeBuilder(undefined));

      await expect(build({ accountId: ACCOUNT_ID, salt: SALT })).rejects.toBeInstanceOf(
        MultisigAuthArgsMissingError,
      );
    });
  }
});
