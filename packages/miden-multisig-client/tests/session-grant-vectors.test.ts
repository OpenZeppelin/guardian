import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { p256 } from '@noble/curves/nist.js';
import { hashTypedData } from 'viem';
import { describe, expect, it } from 'vitest';

import {
  SESSION_GRANT_SCOPE,
  formatUtcSeconds,
  sessionDomainTag,
  sessionGrantDigest,
  sessionLogoutDigest,
  sessionLogoutDomainTag,
  sessionRevokeAllDigest,
  sessionRevokeAllDomainTag,
} from '../src/session/grant.js';
import {
  guardianSessionRevokeAllTypedData,
  guardianSessionTypedData,
  typedDataDigest,
} from '../src/utils/eip712.js';
import { bytesToHex, hexToBytes } from '../src/utils/encoding.js';

// Cross-language parity with `crates/shared/src/session_grant.rs`, plus an
// independent EIP-712 implementation (viem) for every typed-data digest.
// Regenerate the fixture with `GUARDIAN_REGEN_SESSION_FIXTURES=1 cargo test -p
// guardian-shared --test session_grant_vectors`.

interface Fixture {
  schema: string;
  scope: string;
  domain_tag_hex: string;
  logout_domain_tag_hex: string;
  revoke_all_domain_tag_hex: string;
  grants: Array<{
    name: string;
    signer_commitment_hex: string;
    session_public_key_hex: string;
    origin: string;
    issued_at: number;
    expires_at: number;
    guardian_commitment_hex: string;
    network: string;
    expected_expires: string;
    expected_digest_hex: string;
    expected_eip712_digest_hex: string;
  }>;
  logouts: Array<{
    name: string;
    session_public_key_hex: string;
    timestamp_ms: number;
    expected_digest_hex: string;
  }>;
  revoke_alls: Array<{
    name: string;
    signer_commitment_hex: string;
    timestamp_ms: number;
    expected_digest_hex: string;
    expected_eip712_digest_hex: string;
  }>;
  session_keys: Array<{
    name: string;
    secret_hex: string;
    expected_public_key_hex: string;
    message_hex: string;
    signature_hex: string;
  }>;
}

const fixture = JSON.parse(
  readFileSync(
    fileURLToPath(
      new URL(
        '../../../crates/shared/tests/fixtures/session_grant_vectors.json',
        import.meta.url,
      ),
    ),
    'utf8',
  ),
) as Fixture;

const domain = { name: 'Guardian Session', version: '1' } as const;

/** viem's digest of the same typed data, with integers as bigints. */
function viemDigest(
  primaryType: string,
  fields: Array<{ name: string; type: string }>,
  message: Record<string, string | bigint>,
): string {
  return hashTypedData({
    domain,
    types: { [primaryType]: fields },
    primaryType,
    message,
  } as unknown as Parameters<typeof hashTypedData>[0]);
}

describe('session grant parity with the Rust crate', () => {
  it('uses the same domain tags and scope', () => {
    expect(fixture.schema).toBe('guardian.session_grant.v1');
    expect(fixture.scope).toBe(SESSION_GRANT_SCOPE);
    expect(sessionDomainTag().toHex()).toBe(fixture.domain_tag_hex);
    expect(sessionLogoutDomainTag().toHex()).toBe(fixture.logout_domain_tag_hex);
    expect(sessionRevokeAllDomainTag().toHex()).toBe(fixture.revoke_all_domain_tag_hex);
  });

  for (const vector of fixture.grants) {
    it(`matches grant vector ${vector.name}`, () => {
      const grant = {
        signerCommitment: vector.signer_commitment_hex,
        sessionPublicKey: vector.session_public_key_hex,
        origin: vector.origin,
        issuedAt: vector.issued_at,
        expiresAt: vector.expires_at,
        guardianCommitment: vector.guardian_commitment_hex,
        network: vector.network,
      };

      expect(formatUtcSeconds(vector.expires_at)).toBe(vector.expected_expires);
      expect(sessionGrantDigest(grant).toHex()).toBe(vector.expected_digest_hex);

      const typed = guardianSessionTypedData(grant);
      expect(typed.message.scope).toBe(SESSION_GRANT_SCOPE);
      expect(typed.message.expires).toBe(vector.expected_expires);
      expect(bytesToHex(typedDataDigest(typed))).toBe(vector.expected_eip712_digest_hex);
      expect(
        viemDigest('GuardianSession', typed.types.GuardianSession, {
          signer: vector.signer_commitment_hex,
          sessionKey: vector.session_public_key_hex,
          origin: vector.origin,
          issuedAt: BigInt(vector.issued_at),
          expiresAt: BigInt(vector.expires_at),
          expires: vector.expected_expires,
          scope: fixture.scope,
          guardianKey: vector.guardian_commitment_hex,
          network: vector.network,
        }),
      ).toBe(vector.expected_eip712_digest_hex);
    });
  }

  for (const vector of fixture.logouts) {
    it(`matches logout vector ${vector.name}`, () => {
      expect(
        sessionLogoutDigest(vector.session_public_key_hex, vector.timestamp_ms).toHex(),
      ).toBe(vector.expected_digest_hex);
    });
  }

  for (const vector of fixture.revoke_alls) {
    it(`matches revoke-all vector ${vector.name}`, () => {
      expect(
        sessionRevokeAllDigest(vector.signer_commitment_hex, vector.timestamp_ms).toHex(),
      ).toBe(vector.expected_digest_hex);

      const typed = guardianSessionRevokeAllTypedData(
        vector.signer_commitment_hex,
        vector.timestamp_ms,
      );
      expect(bytesToHex(typedDataDigest(typed))).toBe(vector.expected_eip712_digest_hex);
      expect(
        viemDigest('GuardianSessionRevokeAll', typed.types.GuardianSessionRevokeAll, {
          signer: vector.signer_commitment_hex,
          timestamp: BigInt(vector.timestamp_ms),
        }),
      ).toBe(vector.expected_eip712_digest_hex);
    });
  }

  for (const vector of fixture.session_keys) {
    it(`verifies Rust session-key signature ${vector.name}`, () => {
      const secret = hexToBytes(vector.secret_hex);
      expect(bytesToHex(p256.getPublicKey(secret, true))).toBe(vector.expected_public_key_hex);
      expect(
        p256.verify(
          hexToBytes(vector.signature_hex),
          hexToBytes(vector.message_hex),
          hexToBytes(vector.expected_public_key_hex),
          { prehash: true, lowS: false },
        ),
      ).toBe(true);
    });
  }

  it('rejects malformed session keys, empty lifetimes and negative timestamps', () => {
    const grant = {
      signerCommitment: fixture.grants[0].signer_commitment_hex,
      sessionPublicKey: '0x04' + '11'.repeat(32),
      origin: '',
      issuedAt: 100,
      expiresAt: 200,
      guardianCommitment: fixture.grants[0].guardian_commitment_hex,
      network: 'devnet',
    };
    expect(() => sessionGrantDigest(grant)).toThrow(/compressed P-256/);
    expect(() =>
      sessionGrantDigest({
        ...grant,
        sessionPublicKey: fixture.grants[0].session_public_key_hex,
        expiresAt: 100,
      }),
    ).toThrow(/after issuedAt/);
    expect(() =>
      guardianSessionRevokeAllTypedData(fixture.grants[0].signer_commitment_hex, -1),
    ).toThrow(/non-negative/);
  });
});
