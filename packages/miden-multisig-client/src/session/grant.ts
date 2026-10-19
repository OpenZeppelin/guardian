import type { SessionGrantFields } from '@openzeppelin/guardian-client';
import { Felt, FeltArray, Rpo256, Word } from '@miden-sdk/miden-sdk';
import { domainTagWord, feltFromU64Reduced } from '../utils/felt.js';
import { hexToBytes } from '../utils/encoding.js';

/**
 * Session grant, logout and revoke-all digests (issue #219). MUST produce
 * byte-identical digests to `crates/shared/src/session_grant.rs`; parity is
 * verified against `crates/shared/tests/fixtures/session_grant_vectors.json`.
 */

/**
 * The scope every v1 grant states, shown by the wallet: the delegated signer
 * acts for every account its wallet key cosigns on this Guardian, including
 * accounts the key is added to later, until the grant expires.
 */
export const SESSION_GRANT_SCOPE =
  'Every account this signer cosigns on this Guardian, now or later, until this grant expires';

const DOMAIN_TAG_BYTES = new TextEncoder().encode('guardian.session.v1');
const LOGOUT_DOMAIN_TAG_BYTES = new TextEncoder().encode('guardian.session.logout.v1');
const REVOKE_ALL_DOMAIN_TAG_BYTES = new TextEncoder().encode('guardian.session.revoke_all.v1');

let cachedDomainTag: Word | null = null;
let cachedLogoutDomainTag: Word | null = null;
let cachedRevokeAllDomainTag: Word | null = null;

export function sessionDomainTag(): Word {
  cachedDomainTag ??= domainTagWord(DOMAIN_TAG_BYTES);
  return cachedDomainTag;
}

export function sessionLogoutDomainTag(): Word {
  cachedLogoutDomainTag ??= domainTagWord(LOGOUT_DOMAIN_TAG_BYTES);
  return cachedLogoutDomainTag;
}

export function sessionRevokeAllDomainTag(): Word {
  cachedRevokeAllDomainTag ??= domainTagWord(REVOKE_ALL_DOMAIN_TAG_BYTES);
  return cachedRevokeAllDomainTag;
}

/**
 * `[len, u32_le_chunk...]` felts: injective for arbitrary bytes, including
 * public keys. Mirrors `felts_from_bytes` in the Rust crate.
 */
function feltsFromBytes(bytes: Uint8Array): Felt[] {
  const felts: Felt[] = [new Felt(BigInt(bytes.length))];
  for (let offset = 0; offset < bytes.length; offset += 4) {
    let value = 0;
    for (let i = 0; i < 4; i += 1) {
      const byte = offset + i < bytes.length ? bytes[offset + i] : 0;
      value += byte * 2 ** (8 * i);
    }
    felts.push(new Felt(BigInt(value)));
  }
  return felts;
}

function hashBytes(bytes: Uint8Array): Word {
  return Rpo256.hashElements(new FeltArray(feltsFromBytes(bytes)));
}

/** The canonical readable expiry, `YYYY-MM-DD HH:MM:SS UTC`, derived from `expiresAt`. */
export function formatUtcSeconds(unixSeconds: number): string {
  const iso = new Date(unixSeconds * 1000).toISOString();
  return `${iso.slice(0, 10)} ${iso.slice(11, 19)} UTC`;
}

function sessionPublicKeyBytes(hex: string): Uint8Array {
  const bytes = hexToBytes(hex);
  if (bytes.length !== 33 || (bytes[0] !== 0x02 && bytes[0] !== 0x03)) {
    throw new Error('Session public key must be a 33-byte SEC1 compressed P-256 point');
  }
  return bytes;
}

/**
 * Digest signed by Falcon and raw ECDSA wallets: the domain tag followed by
 * every grant field in the clear, in the order the wallet displays them.
 */
export function sessionGrantDigest(grant: SessionGrantFields): Word {
  if (grant.expiresAt <= grant.issuedAt) {
    throw new Error('Session grant expiresAt must be after issuedAt');
  }
  const encoder = new TextEncoder();
  const felts: Felt[] = [
    ...sessionDomainTag().toFelts(),
    ...Word.fromHex(grant.signerCommitment).toFelts(),
    ...feltsFromBytes(sessionPublicKeyBytes(grant.sessionPublicKey)),
    ...hashBytes(encoder.encode(grant.origin)).toFelts(),
    feltFromU64Reduced(BigInt(grant.issuedAt)),
    feltFromU64Reduced(BigInt(grant.expiresAt)),
    ...hashBytes(encoder.encode(formatUtcSeconds(grant.expiresAt))).toFelts(),
    ...hashBytes(encoder.encode(SESSION_GRANT_SCOPE)).toFelts(),
    ...Word.fromHex(grant.guardianCommitment).toFelts(),
    ...hashBytes(encoder.encode(grant.network)).toFelts(),
  ];
  return Rpo256.hashElements(new FeltArray(felts));
}

/**
 * The grant as the label/value pairs a wallet shows, in signing order. Raw
 * (Falcon or raw ECDSA) wallets display only a hash, so show these to the
 * user before asking such a wallet to sign.
 */
export function describeSessionGrant(
  grant: SessionGrantFields,
): Array<{ label: string; value: string }> {
  return [
    { label: 'Signer', value: grant.signerCommitment },
    { label: 'Session key', value: grant.sessionPublicKey },
    { label: 'Website', value: grant.origin || '(none: outside a browser)' },
    { label: 'Valid from', value: formatUtcSeconds(grant.issuedAt) },
    { label: 'Expires', value: formatUtcSeconds(grant.expiresAt) },
    { label: 'Scope', value: SESSION_GRANT_SCOPE },
    { label: 'Guardian key', value: grant.guardianCommitment },
    { label: 'Network', value: grant.network },
  ];
}

/** Digest of the account-less, session-key-signed `POST /session/logout`. */
export function sessionLogoutDigest(sessionPublicKey: string, timestampMs: number): Word {
  const felts: Felt[] = [
    ...sessionLogoutDomainTag().toFelts(),
    feltFromU64Reduced(BigInt(timestampMs)),
    ...feltsFromBytes(sessionPublicKeyBytes(sessionPublicKey)),
  ];
  return Rpo256.hashElements(new FeltArray(felts));
}

/**
 * Digest of the account-less, wallet-signed `POST /session/revoke-all`, which
 * ends every session of `signerCommitment` on the Guardian.
 */
export function sessionRevokeAllDigest(signerCommitment: string, timestampMs: number): Word {
  const felts: Felt[] = [
    ...sessionRevokeAllDomainTag().toFelts(),
    feltFromU64Reduced(BigInt(timestampMs)),
    ...Word.fromHex(signerCommitment).toFelts(),
  ];
  return Rpo256.hashElements(new FeltArray(felts));
}
