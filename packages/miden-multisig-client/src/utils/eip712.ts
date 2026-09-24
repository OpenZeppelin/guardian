import { keccak_256 } from '@noble/hashes/sha3.js';
import { bytesToHex, hexToBytes } from './encoding.js';

type TypedField = { name: string; type: string };
const EIP191_PREFIX = 0x19;
const EIP712_VERSION = 0x01;
const DOMAIN_VERSION = '1';

const domainFields: TypedField[] = [
  { name: 'name', type: 'string' },
  { name: 'version', type: 'string' },
];

function typedData(name: string, primaryType: string, fieldName: string, value: string) {
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

export function typedDataDigest(data: ReturnType<typeof typedData>): Uint8Array {
  const domainType = new TextEncoder().encode('EIP712Domain(string name,string version)');
  const primaryType = new TextEncoder().encode(`${data.primaryType}(bytes32 ${Object.keys(data.message)[0]})`);
  const domain = new Uint8Array(96);
  domain.set(keccak_256(domainType));
  domain.set(keccak_256(new TextEncoder().encode(data.domain.name)), 32);
  domain.set(keccak_256(new TextEncoder().encode(data.domain.version)), 64);
  const struct = new Uint8Array(64);
  struct.set(keccak_256(primaryType));
  struct.set(hexToBytes(Object.values(data.message)[0]), 32);
  const preimage = new Uint8Array(66);
  preimage.set([EIP191_PREFIX, EIP712_VERSION]);
  preimage.set(keccak_256(domain), 2);
  preimage.set(keccak_256(struct), 34);
  return keccak_256(preimage);
}
