import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { AuthSecretKey } from '@miden-sdk/miden-sdk';

import { bytesToHex } from '../../src/utils/encoding.js';

const here = dirname(fileURLToPath(import.meta.url));

/**
 * The server's committed test fixtures. The account they describe binds one
 * specific guardian identity, so the server under test must carry the matching
 * acknowledgement key.
 */
const FIXTURE_DIR = join(here, '../../../../crates/server/src/testing/fixtures');

/**
 * `AuthSecretKey` serializes as a one-byte scheme tag followed by the key. The
 * fixtures store the bare Falcon key, so the tag has to be prepended. Tag 1
 * deserializes without error and yields a different key, so this value is not
 * something to guess at.
 */
const FALCON_SCHEME_TAG = 2;

function readFixture(name: string): unknown {
  return JSON.parse(readFileSync(join(FIXTURE_DIR, name), 'utf8'));
}

export interface FixtureAccount {
  readonly account_id: string;
  readonly data: string;
}

export interface ServerFixtures {
  readonly accountId: string;
  /**
   * The commitment the fixture account carries before any delta is applied,
   * which is the state the deterministic profile registers and reads back.
   */
  readonly initialCommitment: string;
  readonly account: FixtureAccount;
  readonly cosignerCommitments: readonly string[];
  readonly signerKey: AuthSecretKey;
  readonly signerCommitment: string;
}

export interface OperatorKey {
  readonly secretKey: AuthSecretKey;
  readonly commitment: string;
  readonly publicKey: string;
}

/**
 * Operator identities for the dashboard scenarios, taken from the server
 * fixtures so the allowlist the stack writes and the keys used here cannot
 * drift apart. The restricted operator holds no permissions.
 */
export function operatorKey(which: 'reader' | 'restricted'): OperatorKey {
  const keys = readFixture('keys.json') as Record<string, string>;
  const index = which === 'reader' ? 4 : 5;
  const raw = Uint8Array.from(Buffer.from(keys[`signer_${index}_secret_key`], 'hex'));
  const secretKey = AuthSecretKey.deserialize(Uint8Array.from([FALCON_SCHEME_TAG, ...raw]));
  const publicKey = secretKey.publicKey();
  return {
    secretKey,
    commitment: publicKey.toCommitment().toHex(),
    publicKey: bytesToHex(publicKey.serialize().slice(1)),
  };
}

export function loadServerFixtures(): ServerFixtures {
  const keys = readFixture('keys.json') as Record<string, string>;
  const commitments = readFixture('commitments.json') as Record<string, string>;
  const account = readFixture('account.json') as FixtureAccount;

  const raw = Uint8Array.from(Buffer.from(keys.signer_1_secret_key, 'hex'));
  const signerKey = AuthSecretKey.deserialize(Uint8Array.from([FALCON_SCHEME_TAG, ...raw]));

  const cosignerCommitments = [1, 2, 3].map((index) => {
    const commitment = keys[`signer_${index}_commitment`];
    if (!commitment) throw new Error(`keys.json has no signer_${index}_commitment`);
    return commitment;
  });

  if (!commitments.initial_commitment) {
    throw new Error('commitments.json has no initial_commitment');
  }

  return {
    accountId: commitments.account_id,
    initialCommitment: commitments.initial_commitment,
    account,
    cosignerCommitments,
    signerKey,
    signerCommitment: keys.signer_1_commitment,
  };
}
