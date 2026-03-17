import { describe, expect, it, vi, beforeEach } from 'vitest';
import { EcdsaSigner } from './ecdsa.js';

vi.mock('@miden-sdk/miden-sdk', () => {
  const mockSignature = {
    serialize: () => new Uint8Array([0, 1, 2, 3, 4, 5]),
  };

  const mockPublicKey = {
    toCommitment: () => ({
      toHex: () => '0x' + 'a'.repeat(64),
    }),
    serialize: () => new Uint8Array([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
  };

  const mockSecretKey = {
    publicKey: vi.fn().mockReturnValue(mockPublicKey),
    sign: vi.fn().mockReturnValue(mockSignature),
  };

  return {
    AuthSecretKey: vi.fn(),
    Word: {
      fromHex: vi.fn((hex: string) => ({
        toHex: () => hex,
        toFelts: () => [1, 2, 3, 4],
      })),
    },
    AccountId: {
      fromHex: vi.fn(() => ({
        prefix: () => ({ asInt: () => BigInt(1) }),
        suffix: () => ({ asInt: () => BigInt(2) }),
      })),
    },
    Felt: vi.fn().mockImplementation((value: bigint) => ({ value })),
    FeltArray: vi.fn().mockImplementation((elements: unknown[]) => elements),
    Rpo256: {
      hashElements: vi.fn().mockReturnValue({
        toHex: () => '0x' + 'b'.repeat(64),
        toFelts: () => [1, 2, 3, 4],
      }),
    },
  };
});

describe('EcdsaSigner', () => {
  let mockSecretKey: {
    publicKey: ReturnType<typeof vi.fn>;
    sign: ReturnType<typeof vi.fn>;
  };
  let signer: EcdsaSigner;

  beforeEach(async () => {
    vi.clearAllMocks();
    mockSecretKey = {
      publicKey: vi.fn().mockReturnValue({
        toCommitment: () => ({ toHex: () => '0x' + 'a'.repeat(64) }),
        serialize: () => new Uint8Array([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
      }),
      sign: vi.fn().mockReturnValue({
        serialize: () => new Uint8Array([0, 1, 2, 3, 4, 5]),
      }),
    };
    signer = new EcdsaSigner(mockSecretKey as never);
  });

  it('signs request-bound auth messages for ECDSA', async () => {
    const { Rpo256 } = await import('@miden-sdk/miden-sdk');

    const signature = await signer.signRequest(
      '0x' + 'a'.repeat(30),
      1700000000,
      { toBytes: () => new Uint8Array([1, 2, 3, 4]) } as never,
    );

    expect(signature).toBe('0x0102030405');
    expect(mockSecretKey.sign).toHaveBeenCalled();
    expect(Rpo256.hashElements).toHaveBeenCalledTimes(2);
  });
});
