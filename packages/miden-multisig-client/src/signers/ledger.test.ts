import { describe, expect, it, vi } from 'vitest';
import { secp256k1 } from '@noble/curves/secp256k1';
import { keccak_256 } from '@noble/hashes/sha3.js';
import { LedgerSigner } from './ledger.js';
import { bytesToHex } from '../utils/encoding.js';
import { typedDataDigest } from '../utils/eip712.js';

vi.mock('../utils/signature.js', () => ({
  tryComputeEcdsaCommitmentHex: () => '0x' + 'ab'.repeat(32),
}));
vi.mock('../utils/digest.js', () => ({
  AuthDigest: {
    fromCommitmentHex: () => ({ toFelts: () => [1n, 2n, 3n, 4n].map(value => ({ asInt: () => value })) }),
    fromRequest: () => ({ toFelts: () => [5n, 6n, 7n, 8n].map(value => ({ asInt: () => value })) }),
  },
}));

describe('LedgerSigner', () => {
  const privateKey = new Uint8Array(32).fill(7);
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
      const signature = secp256k1.sign(typedDataDigest(data), privateKey);
      return bytesToHex(new Uint8Array([...signature.toCompactRawBytes(), signature.recovery + 27]));
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

  it('signs a separate Guardian request-auth typed message', async () => {
    const request = vi.fn(async ({ params }: { method: string; params: unknown[] }) => {
      const data = JSON.parse(params[1] as string);
      expect(data.primaryType).toBe('GuardianRequest');
      expect(data.domain.name).toBe('Guardian Request');
      const signature = secp256k1.sign(typedDataDigest(data), privateKey);
      return bytesToHex(new Uint8Array([...signature.toCompactRawBytes(), signature.recovery + 27]));
    });
    const signer = new LedgerSigner({ request }, publicKey, address);
    const signature = await signer.signRequest('0x' + 'aa'.repeat(15), 1_700_000_000, {
      toBytes: () => new Uint8Array([1]),
    } as never);

    expect(signer.requestAuthFormat).toBe('eip712');
    expect(signature).toMatch(/^0x[0-9a-f]{130}$/);
    expect(request).toHaveBeenCalledTimes(1);
  });

  it('discovers the public key from the selected Ledger account', async () => {
    const request = vi.fn(async ({ method, params }: { method: string; params: unknown[] }) => {
      if (method === 'eth_requestAccounts') return [address];
      expect(method).toBe('eth_signTypedData_v4');
      const data = JSON.parse(params[1] as string);
      expect(data.primaryType).toBe('GuardianKeyDiscovery');
      const signature = secp256k1.sign(typedDataDigest(data), privateKey);
      return bytesToHex(new Uint8Array([...signature.toCompactRawBytes(), signature.recovery + 27]));
    });

    const signer = await LedgerSigner.connect({ request });
    expect(signer.publicKey).toBe(publicKey);
    expect(signer.address).toBe(address);
    expect(request).toHaveBeenCalledTimes(2);
  });
});
