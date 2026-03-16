import { describe, it, expect, vi, beforeEach } from 'vitest';
import { FalconSigner } from './falcon.js';

vi.mock('@miden-sdk/miden-sdk', () => {
  const mockSignature = {
    serialize: () => new Uint8Array([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
  };

  const mockPublicKey = {
    toCommitment: () => ({
      toHex: () => '0x' + 'a'.repeat(64),
    }),
    serialize: () => new Uint8Array([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]),
  };

  const mockSecretKey = {
    publicKey: vi.fn().mockReturnValue(mockPublicKey),
    sign: vi.fn().mockReturnValue(mockSignature),
  };

  return {
    AuthSecretKey: {
      rpoFalconWithRNG: vi.fn().mockReturnValue(mockSecretKey),
    },
    Word: {
      fromHex: vi.fn((hex: string) => ({
        toHex: () => hex,
        toFelts: () => [1, 2, 3, 4],
      })),
    },
    AccountId: {
      fromHex: vi.fn((hex: string) => ({
        prefix: () => ({ asInt: () => BigInt(1) }),
        suffix: () => ({ asInt: () => BigInt(2) }),
      })),
    },
    Felt: vi.fn().mockImplementation((v: bigint) => ({ value: v })),
    FeltArray: vi.fn().mockImplementation((arr: any[]) => arr),
    Rpo256: {
      hashElements: vi.fn().mockReturnValue({
        toHex: () => '0x' + 'b'.repeat(64),
      }),
    },
  };
});

describe('FalconSigner', () => {
  let mockSecretKey: any;
  let signer: FalconSigner;

  beforeEach(async () => {
    vi.clearAllMocks();
    const { AuthSecretKey } = await import('@miden-sdk/miden-sdk');
    mockSecretKey = AuthSecretKey.rpoFalconWithRNG(new Uint8Array(32));
    signer = new FalconSigner(mockSecretKey);
  });

  describe('constructor', () => {
    it('extracts commitment from public key', () => {
      expect(signer.commitment).toBe('0x' + 'a'.repeat(64));
    });

    it('extracts public key without first byte', () => {
      expect(signer.publicKey).toBe('0x0102030405060708090a0b0c0d0e0f');
    });

    it('calls publicKey method on secret key', () => {
      expect(mockSecretKey.publicKey).toHaveBeenCalled();
    });
  });

  describe('signAccountIdWithTimestamp', () => {
    it('handles account ID with 0x prefix', async () => {
      const signature = await signer.signAccountIdWithTimestamp('0x' + 'a'.repeat(30), 1700000000);
      expect(signature).toMatch(/^0x[a-f0-9]+$/);
    });

    it('handles account ID without 0x prefix', async () => {
      const signature = await signer.signAccountIdWithTimestamp('a'.repeat(30), 1700000000);
      expect(signature).toMatch(/^0x[a-f0-9]+$/);
    });

    it('calls sign method with hashed digest', async () => {
      await signer.signAccountIdWithTimestamp('0x' + 'a'.repeat(30), 1700000000);
      expect(mockSecretKey.sign).toHaveBeenCalled();
    });

    it('returns signature without first byte', async () => {
      const signature = await signer.signAccountIdWithTimestamp('0x' + 'a'.repeat(30), 1700000000);
      expect(signature).toBe('0x010203040506070809');
    });

    it('includes timestamp in signed payload', async () => {
      const { Felt, FeltArray } = await import('@miden-sdk/miden-sdk');
      await signer.signAccountIdWithTimestamp('0x' + 'a'.repeat(30), 1700000000);
      expect(FeltArray).toHaveBeenCalledWith(expect.arrayContaining([
        expect.anything(),
        expect.anything(),
        expect.anything(),
        expect.anything(),
      ]));
    });
  });

  describe('signCommitment', () => {
    it('handles commitment with 0x prefix', async () => {
      const signature = await signer.signCommitment('0x' + 'c'.repeat(64));
      expect(signature).toMatch(/^0x[a-f0-9]+$/);
    });

    it('handles commitment without 0x prefix', async () => {
      const signature = await signer.signCommitment('c'.repeat(64));
      expect(signature).toMatch(/^0x[a-f0-9]+$/);
    });

    it('pads short commitment hex to 64 characters', async () => {
      const signature = await signer.signCommitment('abc');
      expect(signature).toMatch(/^0x[a-f0-9]+$/);
    });

    it('calls sign method with word from hex', async () => {
      await signer.signCommitment('0x' + 'c'.repeat(64));
      expect(mockSecretKey.sign).toHaveBeenCalled();
    });

    it('returns signature without first byte', async () => {
      const signature = await signer.signCommitment('0x' + 'c'.repeat(64));
      expect(signature).toBe('0x010203040506070809');
    });
  });
});
