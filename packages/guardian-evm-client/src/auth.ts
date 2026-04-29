import type { GuardianTypedData } from './typed-data.js';
import { normalizeBytes32, normalizeEvmAddress, normalizeSignature } from './encoding.js';

export interface Eip1193Provider {
  request(args: { method: string; params?: unknown[] }): Promise<unknown>;
}

export async function requestWalletAddress(provider: Eip1193Provider): Promise<`0x${string}`> {
  const accounts = await provider.request({ method: 'eth_requestAccounts' });
  if (!Array.isArray(accounts) || typeof accounts[0] !== 'string') {
    throw new Error('Wallet did not return an account');
  }
  return normalizeEvmAddress(accounts[0]);
}

export async function signTypedData(
  provider: Eip1193Provider,
  signerAddress: string,
  typedData: GuardianTypedData
): Promise<`0x${string}`> {
  const signer = normalizeEvmAddress(signerAddress);
  const signature = await provider.request({
    method: 'eth_signTypedData_v4',
    params: [signer, JSON.stringify(typedData)],
  });
  if (typeof signature !== 'string') {
    throw new Error('Wallet returned a non-string EIP-712 signature');
  }
  return normalizeSignature(signature);
}

export async function signProposalHash(
  provider: Eip1193Provider,
  signerAddress: string,
  hash: string
): Promise<`0x${string}`> {
  const signer = normalizeEvmAddress(signerAddress);
  const normalizedHash = normalizeBytes32(hash, 'hash');
  const signature = await provider.request({
    method: 'eth_sign',
    params: [signer, normalizedHash],
  });
  if (typeof signature !== 'string') {
    throw new Error('Wallet returned a non-string hash signature');
  }
  return normalizeSignature(signature);
}
