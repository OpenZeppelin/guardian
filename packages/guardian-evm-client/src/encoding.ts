import { getAddress, isHex, type Hex } from 'viem';

export function normalizeEvmAddress(address: string): `0x${string}` {
  return getAddress(address).toLowerCase() as `0x${string}`;
}

export function normalizeBytes32(value: string, field: string): Hex {
  if (!isHex(value) || value.length !== 66) {
    throw new Error(`${field} must be a 32-byte 0x-prefixed hex string`);
  }
  return value.toLowerCase() as Hex;
}

export function normalizeSignature(value: string): Hex {
  if (!isHex(value) || value.length !== 132) {
    throw new Error('signature must be a 65-byte 0x-prefixed hex string');
  }
  return value.toLowerCase() as Hex;
}

export function normalizeUint256String(value: string): string {
  const trimmed = value.trim();
  if (trimmed.startsWith('0x')) {
    if (!isHex(trimmed) || trimmed.length > 66) {
      throw new Error('nonce must be a uint256 decimal string or 0x-prefixed hex string');
    }
    return trimmed.toLowerCase();
  }
  if (!/^[0-9]+$/.test(trimmed)) {
    throw new Error('nonce must be a uint256 decimal string or 0x-prefixed hex string');
  }
  return trimmed;
}
