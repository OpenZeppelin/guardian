import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

import { Account } from '@miden-sdk/miden-sdk';

import { AccountInspector } from '../src/inspector.js';

interface SerializedAccountFixture {
  account_hex: string;
  threshold: number;
  signer_commitments: string[];
  guardian_commitment: string;
}

let cachedFixture: SerializedAccountFixture | null = null;

function loadFixture(): SerializedAccountFixture {
  if (cachedFixture) {
    return cachedFixture;
  }

  const repoRoot = fileURLToPath(new URL('../../../', import.meta.url));
  const output = execFileSync(
    'cargo',
    ['run', '--quiet', '--example', 'serialized_account', '-p', 'miden-multisig-client'],
    {
      cwd: repoRoot,
      encoding: 'utf8',
    },
  );

  cachedFixture = JSON.parse(output) as SerializedAccountFixture;
  return cachedFixture;
}

function hexToBytes(hex: string): Uint8Array {
  const normalized = hex.replace(/^0x/, '');
  const bytes = new Uint8Array(normalized.length / 2);
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(normalized.slice(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

// Round-trips a Rust-built account through the real WASM SDK: proves the
// Rust writer and the TS readers agree on the storage layout, and exercises
// the accessors against the SDK's actual storage read semantics (empty word
// for missing map keys, undefined for absent slots) instead of mocks.
describe('AccountInspector round-trip against a Rust-built account', () => {
  it('reads the signer commitments in signer-index order', () => {
    const fixture = loadFixture();
    const account = Account.deserialize(hexToBytes(fixture.account_hex));

    expect(AccountInspector.getSignerPublicKeyCommitments(account)).toEqual(
      fixture.signer_commitments,
    );
  });

  it('reads the guardian commitment', () => {
    const fixture = loadFixture();
    const account = Account.deserialize(hexToBytes(fixture.account_hex));

    expect(AccountInspector.getGuardianPublicKeyCommitment(account)).toBe(
      fixture.guardian_commitment,
    );
  });

  it('detects the full config through the lenient inspector', () => {
    const fixture = loadFixture();
    const account = Account.deserialize(hexToBytes(fixture.account_hex));

    const detected = AccountInspector.fromAccount(account);
    expect(detected.threshold).toBe(fixture.threshold);
    expect(detected.numSigners).toBe(fixture.signer_commitments.length);
    expect(detected.signerCommitments).toEqual(fixture.signer_commitments);
    expect(detected.guardianCommitment).toBe(fixture.guardian_commitment);
  });

  // The original #306 failure: the SDK's AccountInterface did not recognize
  // the previous OpenZeppelin auth component, so getPublicKeyCommitments()
  // returned []. With the upstream AuthGuardedMultisig component it must
  // return the approver commitments natively.
  it('getPublicKeyCommitments() works natively on the upstream component (issue #306)', () => {
    const fixture = loadFixture();
    const account = Account.deserialize(hexToBytes(fixture.account_hex));

    const native = account.getPublicKeyCommitments().map((word) => word.toHex());

    expect(native.length).toBeGreaterThan(0);
    for (const signer of fixture.signer_commitments) {
      expect(native).toContain(signer);
    }
  });
});
