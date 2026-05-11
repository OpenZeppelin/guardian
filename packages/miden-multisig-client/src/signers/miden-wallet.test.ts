import { describe, it, expect, vi, beforeEach } from 'vitest';
import { MidenWalletSigner } from './miden-wallet.js';
import type { WalletSigningContext } from './miden-wallet.js';
import type { Signer } from '../types.js';

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

describe('MidenWalletSigner', () => {
  let mockWallet: WalletSigningContext;

  beforeEach(() => {
    vi.clearAllMocks();
    mockWallet = {
      signBytes: vi.fn().mockResolvedValue(new Uint8Array([0x00, 0x01, 0x02, 0x03, 0x04])),
    };
  });

  describe('constructor', () => {
    it('should set commitment, scheme, and publicKey', () => {
      const signer = new MidenWalletSigner(mockWallet, '0xcommitment', 'falcon');
      expect(signer.commitment).toBe('0xcommitment');
      expect(signer.scheme).toBe('falcon');
      expect(signer.publicKey).toBe('0xcommitment');
    });

    it('should use explicit publicKey when provided', () => {
      const signer = new MidenWalletSigner(
        mockWallet,
        '0xcommitment',
        'ecdsa',
        undefined,
        '0xwalletpubkey',
      );
      expect(signer.publicKey).toBe('0xwalletpubkey');
    });

    it('should use localAuthSigner publicKey when provided', () => {
      const localSigner: Signer = {
        commitment: '0xlocal',
        publicKey: '0xlocalpubkey',
        scheme: 'ecdsa',
        signAccountIdWithTimestamp: vi.fn(),
        signCommitment: vi.fn(),
      };
      const signer = new MidenWalletSigner(mockWallet, '0xcommitment', 'ecdsa', localSigner);
      expect(signer.publicKey).toBe('0xlocalpubkey');
    });
  });

  describe('signAccountIdWithTimestamp', () => {
    it('should delegate to localAuthSigner when present', async () => {
      const localSigner: Signer = {
        commitment: '0xlocal',
        publicKey: '0xlocalpubkey',
        scheme: 'ecdsa',
        signAccountIdWithTimestamp: vi.fn().mockResolvedValue('0xlocalsig'),
        signCommitment: vi.fn(),
      };
      const signer = new MidenWalletSigner(mockWallet, '0xcommitment', 'ecdsa', localSigner);
      const result = await signer.signAccountIdWithTimestamp('0x' + 'aa'.repeat(15), 1700000000);
      expect(result).toBe('0xlocalsig');
      expect(localSigner.signAccountIdWithTimestamp).toHaveBeenCalledWith('0x' + 'aa'.repeat(15), 1700000000);
      expect(mockWallet.signBytes).not.toHaveBeenCalled();
    });

    it('should use wallet signing when no localAuthSigner', async () => {
      const signer = new MidenWalletSigner(mockWallet, '0xcommitment', 'falcon');
      const result = await signer.signAccountIdWithTimestamp('0x' + 'aa'.repeat(15), 1700000000);
      expect(mockWallet.signBytes).toHaveBeenCalled();
      expect(result).toMatch(/^0x/);
    });
  });

  describe('signCommitment', () => {
    it('should always use wallet signing', async () => {
      const localSigner: Signer = {
        commitment: '0xlocal',
        publicKey: '0xlocalpubkey',
        scheme: 'ecdsa',
        signAccountIdWithTimestamp: vi.fn(),
        signCommitment: vi.fn(),
      };
      const signer = new MidenWalletSigner(mockWallet, '0xcommitment', 'ecdsa', localSigner);
      await signer.signCommitment('0x' + 'cc'.repeat(32));
      expect(mockWallet.signBytes).toHaveBeenCalled();
    });
  });

  describe('signRequest', () => {
    it('delegates to localAuthSigner when present', async () => {
      const localSigner: Signer = {
        commitment: '0xlocal',
        publicKey: '0xlocalpubkey',
        scheme: 'ecdsa',
        signAccountIdWithTimestamp: vi.fn(),
        signRequest: vi.fn().mockResolvedValue('0xlocalsig'),
        signCommitment: vi.fn(),
      };
      const signer = new MidenWalletSigner(mockWallet, '0xcommitment', 'ecdsa', localSigner);

      const result = await signer.signRequest(
        '0x' + 'aa'.repeat(15),
        1700000000,
        { toBytes: () => new Uint8Array([1, 2, 3, 4]) } as never,
      );

      expect(result).toBe('0xlocalsig');
      expect(localSigner.signRequest).toHaveBeenCalled();
      expect(mockWallet.signBytes).not.toHaveBeenCalled();
    });

    it('uses request-bound auth digest for wallet-backed ECDSA signing', async () => {
      const signer = new MidenWalletSigner(mockWallet, '0xcommitment', 'ecdsa');

      await signer.signRequest(
        '0x' + 'aa'.repeat(15),
        1700000000,
        { toBytes: () => new Uint8Array([1, 2, 3, 4]) } as never,
      );

      expect(mockWallet.signBytes).toHaveBeenCalledWith(expect.any(Uint8Array), 'word');
    });
  });

  describe('signLookupMessage', () => {
    it('delegates to localAuthSigner when present', async () => {
      const localSigner: Signer = {
        commitment: '0xlocal',
        publicKey: '0xlocalpubkey',
        scheme: 'ecdsa',
        signAccountIdWithTimestamp: vi.fn(),
        signCommitment: vi.fn(),
        signLookupMessage: vi.fn().mockResolvedValue('0xlocallookupsig'),
      };
      const signer = new MidenWalletSigner(mockWallet, '0xcommitment', 'ecdsa', localSigner);

      const result = await signer.signLookupMessage('0x' + 'cc'.repeat(32), 1700000000);

      expect(result).toBe('0xlocallookupsig');
      expect(localSigner.signLookupMessage).toHaveBeenCalledWith(
        '0x' + 'cc'.repeat(32),
        1700000000,
      );
      expect(mockWallet.signBytes).not.toHaveBeenCalled();
    });

    it('uses wallet signing over the LookupAuthMessage digest when no localAuthSigner', async () => {
      const signer = new MidenWalletSigner(mockWallet, '0xcommitment', 'falcon');

      const result = await signer.signLookupMessage('0x' + 'cc'.repeat(32), 1700000000);

      expect(mockWallet.signBytes).toHaveBeenCalledTimes(1);
      expect(mockWallet.signBytes).toHaveBeenCalledWith(expect.any(Uint8Array), 'word');
      expect(result).toMatch(/^0x/);
    });

    it('falls back to wallet signing when localAuthSigner does not implement signLookupMessage', async () => {
      const localSigner: Signer = {
        commitment: '0xlocal',
        publicKey: '0xlocalpubkey',
        scheme: 'ecdsa',
        signAccountIdWithTimestamp: vi.fn(),
        signCommitment: vi.fn(),
      };
      const signer = new MidenWalletSigner(mockWallet, '0xcommitment', 'ecdsa', localSigner);

      await signer.signLookupMessage('0x' + 'cc'.repeat(32), 1700000000);

      expect(mockWallet.signBytes).toHaveBeenCalledTimes(1);
    });
  });

  describe('signature byte stripping', () => {
    it('should strip the first byte from wallet signature response', async () => {
      (mockWallet.signBytes as ReturnType<typeof vi.fn>).mockResolvedValue(
        new Uint8Array([0x00, 0xaa, 0xbb, 0xcc]),
      );
      const signer = new MidenWalletSigner(mockWallet, '0xcommitment', 'falcon');
      const result = await signer.signCommitment('0x' + 'cc'.repeat(32));
      expect(result).toBe('0xaabbcc');
    });
  });
});
