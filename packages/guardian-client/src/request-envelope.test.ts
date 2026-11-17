import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { describe, it, expect } from 'vitest';
import { PROTOCOL_LINE, sealTransactionRequest } from './request-envelope.js';

describe('sealTransactionRequest', () => {
  it('seals bytes with this SDK\'s protocol line, checksum and base64 body', async () => {
    const envelope = await sealTransactionRequest(new TextEncoder().encode('abc'));
    expect(envelope).toEqual({
      format_version: 1,
      protocol_line: '0.17',
      checksum: '0xba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
      bytes: 'YWJj',
    });
  });
});

describe('the shared envelope fixture', () => {
  it('seals to the shared vector for this SDK\'s protocol line', async () => {
    const fixture = JSON.parse(
      readFileSync(
        join(dirname(fileURLToPath(import.meta.url)), '../../../fixtures/miden-multisig-client/request-envelope.json'),
        'utf8',
      ),
    );
    const bytes = Uint8Array.from(Buffer.from(fixture.bytes_hex, 'hex'));
    expect(await sealTransactionRequest(bytes)).toEqual(fixture.sealed[PROTOCOL_LINE]);
  });
});
