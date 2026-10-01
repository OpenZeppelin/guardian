import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { describe, it, expect } from 'vitest';
import {
  GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA,
  GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA,
  REQUEST_SERIALIZER_ID,
  approvalExpirationDeltaFor,
  attachmentFor,
  expirationInstructions,
  transactionExpirationDeltaFor,
} from './expiration.js';

const here = dirname(fileURLToPath(import.meta.url));
const rustExpiration = readFileSync(
  join(here, '../../../../crates/miden-multisig-client/src/transaction/expiration.rs'),
  'utf8',
);
const rustEnvelope = readFileSync(join(here, '../../../../crates/shared/src/request_envelope.rs'), 'utf8');
const packageJson = JSON.parse(readFileSync(join(here, '../../package.json'), 'utf8'));

describe('parity with the Rust SDK', () => {
  it('uses the same two bounds and the same serializer id', () => {
    const constantIn = (source: string, name: string) =>
      source.match(new RegExp(`pub const ${name}: [^=]+=\\s*(?:\\w+::new\\()?"?([0-9_.a-z-]+)`))?.[1];
    const constant = (name: string) => constantIn(rustExpiration, name);
    expect(Number(constant('GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA')?.replaceAll('_', ''))).toBe(
      GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA,
    );
    expect(Number(constant('GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA'))).toBe(
      GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA,
    );
    expect(constantIn(rustEnvelope, 'REQUEST_SERIALIZER_ID')).toBe(REQUEST_SERIALIZER_ID);
  });

  it('names the pinned web SDK version as the serializer id', () => {
    expect(packageJson.dependencies['@miden-sdk/miden-sdk']).toBe(REQUEST_SERIALIZER_ID);
  });
});

describe('execution modes', () => {
  it('a self-executed client applies no bound it was not asked for and stores nothing', async () => {
    expect(approvalExpirationDeltaFor('self_executed', undefined)).toBeUndefined();
    expect(approvalExpirationDeltaFor('self_executed', 40)).toBe(40);
    expect(transactionExpirationDeltaFor('self_executed')).toBeUndefined();
    let read = false;
    expect(
      await attachmentFor('self_executed', () => {
        read = true;
        return new Uint8Array();
      }),
    ).toBeUndefined();
    expect(read).toBe(false);
  });

  it('a guardian-executable client defaults both bounds and seals the request', async () => {
    expect(approvalExpirationDeltaFor('guardian_executable', undefined)).toBe(28_800);
    expect(approvalExpirationDeltaFor('guardian_executable', 500)).toBe(500);
    expect(transactionExpirationDeltaFor('guardian_executable')).toBe(256);
    const envelope = await attachmentFor('guardian_executable', () => new TextEncoder().encode('abc'));
    expect(envelope?.serializer_id).toBe('0.17.0-rc.4');
    expect(envelope?.checksum).toBe('0xba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
  });

  it('scripts the transaction expiration only when one is set', () => {
    expect(expirationInstructions(undefined)).toBe('');
    expect(expirationInstructions(256)).toContain('push.256');
    expect(expirationInstructions(256)).toContain('exec.::miden::protocol::tx::update_expiration_block_delta');
  });
});
