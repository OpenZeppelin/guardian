import { keccak_256 } from '@noble/hashes/sha3.js';
import type { SessionGrantFields } from '@openzeppelin/guardian-client';
import { bytesToHex, hexToBytes } from './encoding.js';
import { SESSION_GRANT_SCOPE, formatUtcSeconds } from '../session/grant.js';

type TypedField = { name: string; type: string };

/**
 * EIP-712 typed data over `string`, `bytes`, `bytes32` and `uint64` fields,
 * as Guardian signs it. Integers are decimal strings, as wallets accept them.
 */
export interface GuardianTypedData {
  types: Record<string, TypedField[]>;
  primaryType: string;
  domain: { name: string; version: string };
  message: Record<string, string>;
}

const EIP191_PREFIX = 0x19;
const EIP712_VERSION = 0x01;
const DOMAIN_VERSION = '1';
const SESSION_DOMAIN_NAME = 'Guardian Session';
const UINT64_MAX = 0xffffffffffffffffn;

const domainFields: TypedField[] = [
  { name: 'name', type: 'string' },
  { name: 'version', type: 'string' },
];

function typedData(
  name: string,
  primaryType: string,
  fieldName: string,
  value: string,
): GuardianTypedData {
  return {
    types: {
      EIP712Domain: domainFields,
      [primaryType]: [{ name: fieldName, type: 'bytes32' }],
    },
    primaryType,
    domain: { name, version: DOMAIN_VERSION },
    message: { [fieldName]: value },
  };
}

export function guardianRequestTypedData(requestHash: Uint8Array) {
  return typedData('Guardian Request', 'GuardianRequest', 'requestHash', bytesToHex(requestHash));
}

export function guardianLookupTypedData(lookupHash: Uint8Array) {
  return typedData('Guardian Lookup', 'GuardianLookup', 'lookupHash', bytesToHex(lookupHash));
}

export function midenTransactionTypedData(txSummaryHash: Uint8Array) {
  return typedData('Miden Transaction', 'MidenTransaction', 'txSummaryHash', bytesToHex(txSummaryHash));
}

export function guardianKeyDiscoveryTypedData(challenge: Uint8Array) {
  return typedData('Guardian Key Discovery', 'GuardianKeyDiscovery', 'challenge', bytesToHex(challenge));
}

/**
 * Readable session grant: every value Guardian binds is a top-level member,
 * so the wallet displays the signer, the delegated key, the website, the
 * lifetime (with a readable expiry), the scope, the Guardian key and the
 * network instead of a hash.
 */
export function guardianSessionTypedData(grant: SessionGrantFields): GuardianTypedData {
  return {
    types: {
      EIP712Domain: domainFields,
      GuardianSession: [
        { name: 'signer', type: 'bytes32' },
        { name: 'sessionKey', type: 'bytes' },
        { name: 'origin', type: 'string' },
        { name: 'issuedAt', type: 'uint64' },
        { name: 'expiresAt', type: 'uint64' },
        { name: 'expires', type: 'string' },
        { name: 'scope', type: 'string' },
        { name: 'guardianKey', type: 'bytes32' },
        { name: 'network', type: 'string' },
      ],
    },
    primaryType: 'GuardianSession',
    domain: { name: SESSION_DOMAIN_NAME, version: DOMAIN_VERSION },
    message: {
      signer: grant.signerCommitment,
      sessionKey: grant.sessionPublicKey,
      origin: grant.origin,
      issuedAt: String(grant.issuedAt),
      expiresAt: String(grant.expiresAt),
      expires: formatUtcSeconds(grant.expiresAt),
      scope: SESSION_GRANT_SCOPE,
      guardianKey: grant.guardianCommitment,
      network: grant.network,
    },
  };
}

/**
 * Readable revoke-all: the signer whose sessions end and the request
 * timestamp in milliseconds.
 */
export function guardianSessionRevokeAllTypedData(
  signerCommitment: string,
  timestampMs: number,
): GuardianTypedData {
  if (!Number.isInteger(timestampMs) || timestampMs < 0) {
    throw new Error('Revoke-all timestamp must be a non-negative integer');
  }
  return {
    types: {
      EIP712Domain: domainFields,
      GuardianSessionRevokeAll: [
        { name: 'signer', type: 'bytes32' },
        { name: 'timestamp', type: 'uint64' },
      ],
    },
    primaryType: 'GuardianSessionRevokeAll',
    domain: { name: SESSION_DOMAIN_NAME, version: DOMAIN_VERSION },
    message: { signer: signerCommitment, timestamp: String(timestampMs) },
  };
}

function encodeField(field: TypedField, value: string): Uint8Array {
  switch (field.type) {
    case 'string':
      return keccak_256(new TextEncoder().encode(value));
    case 'bytes':
      return keccak_256(hexToBytes(value));
    case 'bytes32': {
      const bytes = hexToBytes(value);
      if (bytes.length !== 32) {
        throw new Error(`EIP-712 field ${field.name} must be 32 bytes`);
      }
      return bytes;
    }
    case 'uint64': {
      let remaining = BigInt(value);
      if (remaining < 0n || remaining > UINT64_MAX) {
        throw new Error(`EIP-712 field ${field.name} must fit in uint64`);
      }
      const bytes = new Uint8Array(32);
      for (let i = 31; i >= 24; i -= 1) {
        bytes[i] = Number(remaining & 0xffn);
        remaining >>= 8n;
      }
      return bytes;
    }
    default:
      throw new Error(`Unsupported EIP-712 field type: ${field.type}`);
  }
}

export function typedDataDigest(data: GuardianTypedData): Uint8Array {
  const encoder = new TextEncoder();
  const fields = data.types[data.primaryType];
  const typeString = `${data.primaryType}(${fields.map((f) => `${f.type} ${f.name}`).join(',')})`;

  const domain = new Uint8Array(96);
  domain.set(keccak_256(encoder.encode('EIP712Domain(string name,string version)')));
  domain.set(keccak_256(encoder.encode(data.domain.name)), 32);
  domain.set(keccak_256(encoder.encode(data.domain.version)), 64);

  const struct = new Uint8Array(32 * (fields.length + 1));
  struct.set(keccak_256(encoder.encode(typeString)));
  fields.forEach((field, index) => {
    struct.set(encodeField(field, data.message[field.name]), 32 * (index + 1));
  });

  const preimage = new Uint8Array(66);
  preimage.set([EIP191_PREFIX, EIP712_VERSION]);
  preimage.set(keccak_256(domain), 2);
  preimage.set(keccak_256(struct), 34);
  return keccak_256(preimage);
}
