import { describe, expect, it, vi } from 'vitest';
import { secp256k1 } from '@noble/curves/secp256k1';
import { keccak_256 } from '@noble/hashes/sha3.js';
import { privateKeyToAccount } from 'viem/accounts';
import { Eip712Signer, LedgerSigner } from './ledger.js';
import { bytesToHex } from '../utils/encoding.js';

vi.mock('../utils/signature.js', () => ({
  tryComputeEcdsaCommitmentHex: () => '0x' + 'ab'.repeat(32),
}));
vi.mock('../utils/digest.js', () => ({
  AuthDigest: {
    fromCommitmentHex: () => ({ toFelts: () => [1n, 2n, 3n, 4n].map(value => ({ asInt: () => value })) }),
    fromRequest: () => ({ toFelts: () => [5n, 6n, 7n, 8n].map(value => ({ asInt: () => value })) }),
  },
}));
vi.mock('../lookupAuth.js', () => ({
  lookupAuthDigest: () => ({ toFelts: () => [9n, 10n, 11n, 12n].map(value => ({ asInt: () => value })) }),
}));

describe('LedgerSigner', () => {
  it('keeps LedgerSigner as an alias for the EIP-1193 signer', () => {
    expect(LedgerSigner).toBe(Eip712Signer);
  });

  const privateKey = new Uint8Array(32).fill(7);
  const account = privateKeyToAccount(`0x${'07'.repeat(32)}`);
  const publicKey = bytesToHex(secp256k1.getPublicKey(privateKey, true));
  const uncompressed = secp256k1.getPublicKey(privateKey, false);
  const address = bytesToHex(keccak_256(uncompressed.slice(1)).slice(-20));

  it('signs the protocol typed data through eth_signTypedData_v4', async () => {
    const request = vi.fn(async ({ method, params }: { method: string; params: unknown[] }) => {
      expect(method).toBe('eth_signTypedData_v4');
      expect(params[0]).toBe(address);
      const data = JSON.parse(params[1] as string);
      expect(data.primaryType).toBe('MidenTransaction');
      expect(data.domain.name).toBe('Miden Transaction');
      return account.signTypedData(data);
    });
    const signer = new LedgerSigner({ request }, publicKey, address);
    const signature = await signer.signCommitment('0x' + '01'.repeat(32));

    expect(signer.commitment).toBe('0x' + 'ab'.repeat(32));
    expect(signer.proposalMessageFormat).toBe('eip712');
    expect(signature).toMatch(/^0x[0-9a-f]{130}$/);
    expect([0, 1]).toContain(parseInt(signature.slice(-2), 16));
    expect(request).toHaveBeenCalledTimes(1);
  });

  it('rejects a different Ethereum address before requesting a signature', () => {
    expect(() => new LedgerSigner({ request: vi.fn() }, publicKey, '0x' + 'ff'.repeat(20)))
      .toThrow('does not match');
  });

  it('rejects a typed-data signature from another key', async () => {
    const otherAccount = privateKeyToAccount(`0x${'08'.repeat(32)}`);
    const signer = new LedgerSigner({
      request: async ({ params }) => otherAccount.signTypedData(JSON.parse(params[1] as string)),
    }, publicKey, address);

    await expect(signer.signCommitment('0x' + '01'.repeat(32)))
      .rejects.toThrow('different key');
  });

  it('verifies the enrolled key even when the recovery bit differs', async () => {
    const signer = new LedgerSigner({
      request: async ({ params }) => {
        const signed = await account.signTypedData(JSON.parse(params[1] as string));
        return `${signed.slice(0, -2)}${signed.endsWith('1b') ? '1c' : '1b'}`;
      },
    }, publicKey, address);

    await expect(signer.signCommitment('0x' + '01'.repeat(32)))
      .resolves.toMatch(/^0x[0-9a-f]{130}$/);
  });

  it('signs a separate Guardian request-auth typed message', async () => {
    const request = vi.fn(async ({ params }: { method: string; params: unknown[] }) => {
      const data = JSON.parse(params[1] as string);
      expect(data.primaryType).toBe('GuardianRequest');
      expect(data.domain.name).toBe('Guardian Request');
      return account.signTypedData(data);
    });
    const signer = new LedgerSigner({ request }, publicKey, address);
    const signature = await signer.signRequest('0x' + 'aa'.repeat(15), 1_700_000_000, {
      toBytes: () => new Uint8Array([1]),
    } as never);

    expect(signer.requestAuthFormat).toBe('eip712');
    expect(signature).toMatch(/^0x[0-9a-f]{130}$/);
    expect(request).toHaveBeenCalledTimes(1);
  });

  it('signs an account-less Guardian lookup typed message', async () => {
    const request = vi.fn(async ({ params }: { method: string; params: unknown[] }) => {
      const data = JSON.parse(params[1] as string);
      expect(data.primaryType).toBe('GuardianLookup');
      expect(data.domain.name).toBe('Guardian Lookup');
      return account.signTypedData(data);
    });
    const signer = new LedgerSigner({ request }, publicKey, address);
    const signature = await signer.signLookupMessage('0x' + 'ab'.repeat(32), 1_700_000_000);

    expect(signature).toMatch(/^0x[0-9a-f]{130}$/);
    expect(request).toHaveBeenCalledTimes(1);
  });

  it('discovers the public key from the selected Ledger account', async () => {
    const request = vi.fn(async ({ method, params }: { method: string; params: unknown[] }) => {
      if (method === 'eth_requestAccounts') return [address];
      expect(method).toBe('eth_signTypedData_v4');
      const data = JSON.parse(params[1] as string);
      expect(data.primaryType).toBe('GuardianKeyDiscovery');
      return account.signTypedData(data);
    });

    const signer = await LedgerSigner.connect({ request });
    expect(signer.publicKey).toBe(publicKey);
    expect(signer.address).toBe(address);
    expect(request).toHaveBeenCalledTimes(2);
  });
});
