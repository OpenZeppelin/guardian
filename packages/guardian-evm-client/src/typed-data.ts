import type { Hex } from 'viem';
import { normalizeBytes32, normalizeEvmAddress } from './encoding.js';

export type TypedDataField = { name: string; type: string };

export type GuardianTypedData = {
  domain: {
    name: string;
    version: string;
  };
  types: {
    EIP712Domain: TypedDataField[];
    GuardianEvmSession: TypedDataField[];
  };
  primaryType: 'GuardianEvmSession';
  message: {
    wallet: `0x${string}`;
    nonce: Hex;
    issued_at: number;
    expires_at: number;
  };
};

export interface EvmSessionChallenge {
  address: `0x${string}`;
  nonce: Hex;
  issuedAt: number;
  expiresAt: number;
  typedData: GuardianTypedData;
}

export function buildEvmSessionTypedData(input: {
  address: string;
  nonce: string;
  issuedAt: number;
  expiresAt: number;
}): GuardianTypedData {
  return {
    domain: {
      name: 'Guardian EVM Session',
      version: '1',
    },
    types: {
      EIP712Domain: [
        { name: 'name', type: 'string' },
        { name: 'version', type: 'string' },
      ],
      GuardianEvmSession: [
        { name: 'wallet', type: 'address' },
        { name: 'nonce', type: 'bytes32' },
        { name: 'issued_at', type: 'uint64' },
        { name: 'expires_at', type: 'uint64' },
      ],
    },
    primaryType: 'GuardianEvmSession',
    message: {
      wallet: normalizeEvmAddress(input.address),
      nonce: normalizeBytes32(input.nonce, 'nonce'),
      issued_at: input.issuedAt,
      expires_at: input.expiresAt,
    },
  };
}

export function fromServerChallenge(server: ServerChallengeResponse): EvmSessionChallenge {
  const issuedAt = numberField(server.issued_at, 'issued_at');
  const expiresAt = numberField(server.expires_at, 'expires_at');
  const typedData = parseTypedData(server.typed_data, server.address, server.nonce, issuedAt, expiresAt);
  return {
    address: normalizeEvmAddress(server.address),
    nonce: normalizeBytes32(server.nonce, 'nonce'),
    issuedAt,
    expiresAt,
    typedData,
  };
}

interface ServerChallengeResponse {
  address: string;
  nonce: string;
  issued_at: number;
  expires_at: number;
  typed_data?: unknown;
}

function parseTypedData(
  value: unknown,
  address: string,
  nonce: string,
  issuedAt: number,
  expiresAt: number
): GuardianTypedData {
  if (!isRecord(value)) {
    return buildEvmSessionTypedData({ address, nonce, issuedAt, expiresAt });
  }
  const primaryType = value.primaryType;
  if (primaryType !== 'GuardianEvmSession') {
    return buildEvmSessionTypedData({ address, nonce, issuedAt, expiresAt });
  }
  return buildEvmSessionTypedData({ address, nonce, issuedAt, expiresAt });
}

function numberField(value: unknown, field: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value)) {
    throw new Error(`${field} must be a safe integer`);
  }
  return value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}
