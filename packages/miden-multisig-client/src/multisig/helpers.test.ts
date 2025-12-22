import { describe, it, expect, vi, beforeEach } from 'vitest';
import { computeCommitmentFromTxSummary, accountIdToHex } from './helpers.js';

vi.mock('@demox-labs/miden-sdk', () => ({
  Account: {},
  TransactionSummary: {
    deserialize: vi.fn((bytes: Uint8Array) => ({
      toCommitment: () => ({
        toHex: () => '0x' + 'c'.repeat(64),
      }),
    })),
  },
  Word: {},
}));

describe('multisig helpers', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  describe('computeCommitmentFromTxSummary', () => {
    it('computes commitment from base64 tx summary', async () => {
      const base64 = btoa(String.fromCharCode(...new Uint8Array([1, 2, 3])));
      const commitment = computeCommitmentFromTxSummary(base64);

      expect(commitment).toBe('0x' + 'c'.repeat(64));
    });

    it('deserializes the tx summary bytes', async () => {
      const { TransactionSummary } = await import('@demox-labs/miden-sdk');
      const base64 = btoa(String.fromCharCode(...new Uint8Array([4, 5, 6])));

      computeCommitmentFromTxSummary(base64);

      expect(TransactionSummary.deserialize).toHaveBeenCalledWith(
        new Uint8Array([4, 5, 6])
      );
    });

    it('returns hex string with 0x prefix', () => {
      const base64 = btoa(String.fromCharCode(...new Uint8Array([1])));
      const commitment = computeCommitmentFromTxSummary(base64);

      expect(commitment.startsWith('0x')).toBe(true);
    });
  });

  describe('accountIdToHex', () => {
    it('returns account ID string if already hex formatted', () => {
      const mockAccount = {
        id: () => ({
          toString: () => '0xabc123',
          prefix: () => ({ asInt: () => BigInt(1) }),
          suffix: () => ({ asInt: () => BigInt(2) }),
        }),
      };

      const hex = accountIdToHex(mockAccount as any);
      expect(hex).toBe('0xabc123');
    });

    it('handles 0X prefix', () => {
      const mockAccount = {
        id: () => ({
          toString: () => '0Xabc123',
          prefix: () => ({ asInt: () => BigInt(1) }),
          suffix: () => ({ asInt: () => BigInt(2) }),
        }),
      };

      const hex = accountIdToHex(mockAccount as any);
      expect(hex).toBe('0Xabc123');
    });

    it('constructs hex from prefix and suffix if no 0x prefix', () => {
      const mockAccount = {
        id: () => ({
          toString: () => 'not-a-hex',
          prefix: () => ({ asInt: () => BigInt('0x0102030405060708') }),
          suffix: () => ({ asInt: () => BigInt('0x0a0b0c0d0e0f1011') }),
        }),
      };

      const hex = accountIdToHex(mockAccount as any);
      expect(hex).toBe('0x01020304050607080a0b0c0d0e0f10');
    });

    it('pads prefix to 16 hex characters', () => {
      const mockAccount = {
        id: () => ({
          toString: () => 'not-a-hex',
          prefix: () => ({ asInt: () => BigInt(1) }),
          suffix: () => ({ asInt: () => BigInt(0) }),
        }),
      };

      const hex = accountIdToHex(mockAccount as any);
      expect(hex).toBe('0x000000000000000100000000000000');
    });

    it('produces 30-character hex (excluding 0x prefix)', () => {
      const mockAccount = {
        id: () => ({
          toString: () => 'not-a-hex',
          prefix: () => ({ asInt: () => BigInt('0xffffffffffffffff') }),
          suffix: () => ({ asInt: () => BigInt('0xffffffffffffffff') }),
        }),
      };

      const hex = accountIdToHex(mockAccount as any);
      expect(hex.length).toBe(32);
    });
  });
});
