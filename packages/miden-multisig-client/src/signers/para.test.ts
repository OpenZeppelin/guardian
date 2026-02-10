import { describe, it, expect, vi, beforeEach } from 'vitest';
import { ParaSigner } from './para.js';
import type { ParaSigningContext } from './para.js';

vi.mock('@miden-sdk/miden-sdk', () => ({
  Word: {
    fromHex: vi.fn((hex: string) => ({
      toHex: () => hex,
      toFelts: () => [
        { asInt: () => 1n },
        { asInt: () => 2n },
        { asInt: () => 3n },
        { asInt: () => 4n },
      ],
    })),
  },
  AccountId: {
    fromHex: vi.fn(() => ({
      prefix: () => ({ asInt: () => BigInt(1) }),
      suffix: () => ({ asInt: () => BigInt(2) }),
    })),
  },
  Felt: vi.fn().mockImplementation((v: bigint) => ({ value: v })),
  FeltArray: vi.fn().mockImplementation((arr: any[]) => arr),
  Rpo256: {
    hashElements: vi.fn().mockReturnValue({
      toHex: () => '0x' + 'bb'.repeat(32),
      toFelts: () => [
        { asInt: () => 10n },
        { asInt: () => 20n },
        { asInt: () => 30n },
        { asInt: () => 40n },
      ],
    }),
  },
}));

describe('ParaSigner', () => {
  let mockPara: ParaSigningContext;
  const validCompressedKey = '0x02' + 'aa'.repeat(32);
  const validUncompressedKey = '0x04' + 'aa'.repeat(32) + 'bb'.repeat(31) + 'b0';

  beforeEach(() => {
    vi.clearAllMocks();
    mockPara = {
      signMessage: vi.fn().mockResolvedValue({
        signature: '0x' + 'cc'.repeat(64),
      }),
    };
  });

  describe('constructor', () => {
    it('should accept a valid compressed public key', () => {
      const signer = new ParaSigner(mockPara, 'wallet-1', '0xcommitment', validCompressedKey);
      expect(signer.publicKey).toBe(validCompressedKey);
      expect(signer.commitment).toBe('0xcommitment');
      expect(signer.scheme).toBe('ecdsa');
    });

    it('should compress an uncompressed public key', () => {
      const signer = new ParaSigner(mockPara, 'wallet-1', '0xcommitment', validUncompressedKey);
      expect(signer.publicKey).toBe('0x02' + 'aa'.repeat(32));
    });

    it('should throw on invalid public key', () => {
      expect(
        () => new ParaSigner(mockPara, 'wallet-1', '0xcommitment', '0xinvalid'),
      ).toThrow('Invalid ECDSA public key for ParaSigner');
    });
  });

  describe('signAccountIdWithTimestamp', () => {
    it('should call para signMessage and return normalized signature', async () => {
      const signer = new ParaSigner(mockPara, 'wallet-1', '0xcommitment', validCompressedKey);
      const result = await signer.signAccountIdWithTimestamp('0x' + 'aa'.repeat(15), 1700000000);
      expect(mockPara.signMessage).toHaveBeenCalledWith(
        expect.objectContaining({
          walletId: 'wallet-1',
          messageBase64: expect.any(String),
        }),
      );
      expect(result).toMatch(/^0x/);
    });
  });

  describe('signCommitment', () => {
    it('should call para signMessage', async () => {
      const signer = new ParaSigner(mockPara, 'wallet-1', '0xcommitment', validCompressedKey);
      await signer.signCommitment('0x' + 'cc'.repeat(32));
      expect(mockPara.signMessage).toHaveBeenCalled();
    });
  });

  describe('signing denial', () => {
    it('should throw when para returns no signature', async () => {
      (mockPara.signMessage as ReturnType<typeof vi.fn>).mockResolvedValue({ signature: undefined });
      const signer = new ParaSigner(mockPara, 'wallet-1', '0xcommitment', validCompressedKey);
      await expect(signer.signCommitment('0x' + 'cc'.repeat(32))).rejects.toThrow(
        'Para signing was denied by user',
      );
    });
  });
});
