import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import {
  SESSION_TIMESTAMP_MAX_AHEAD_MS,
  SessionTimestamps,
  WriteSequencer,
  isSessionWrite,
  SESSION_REQUEST_AUTH_FORMAT,
  SESSION_REQUEST_VERSION,
  SessionOperation,
  sessionRequestEnvelope,
  type SessionTransport,
} from './session-request.js';

interface RequestVector {
  name: string;
  transport: number;
  operation: number;
  request_word_hex: string;
  envelope_hex: string;
}

interface Fixture {
  auth_format: string;
  version: number;
  operations: Array<{ id: number; name: string; replay_policy: string }>;
  vectors: RequestVector[];
}

const fixture = JSON.parse(
  readFileSync(
    new URL('../../../crates/shared/tests/fixtures/session_request_vectors.json', import.meta.url),
    'utf8'
  )
) as Fixture;

const fromHex = (hex: string): Uint8Array =>
  Uint8Array.from(Buffer.from(hex.replace(/^0x/, ''), 'hex'));

describe('session request envelope', () => {
  it('matches the Rust format, version and operation registry', () => {
    expect(SESSION_REQUEST_AUTH_FORMAT).toBe(fixture.auth_format);
    expect(SESSION_REQUEST_VERSION).toBe(fixture.version);
    expect(
      Object.entries(SessionOperation).map(([name, id]) => ({ id, name }))
    ).toEqual(fixture.operations.map(({ id, name }) => ({ id, name })));
  });

  it('classifies writes as the Rust registry does', () => {
    for (const { id, name, replay_policy } of fixture.operations) {
      expect(isSessionWrite(id as SessionOperation), name).toBe(replay_policy === 'write');
    }
  });

  it('encodes every vector byte for byte', () => {
    for (const vector of fixture.vectors) {
      const envelope = sessionRequestEnvelope(
        vector.transport as SessionTransport,
        vector.operation as SessionOperation,
        fromHex(vector.request_word_hex)
      );
      expect(Buffer.from(envelope).toString('hex'), vector.name).toBe(
        vector.envelope_hex.replace(/^0x/, '')
      );
    }
  });

  it('refuses a request word of the wrong length', () => {
    expect(() =>
      sessionRequestEnvelope(1, SessionOperation.GetState, new Uint8Array(31))
    ).toThrow();
  });
});

describe('SessionTimestamps', () => {
  const NOW = 1_800_000_000_000;

  it('gives equal clock readings distinct timestamps', async () => {
    const timestamps = new SessionTimestamps(() => NOW);
    expect(await timestamps.next('0xa')).toBe(NOW);
    expect(await timestamps.next('0xa')).toBe(NOW + 1);
  });

  it('keeps accounts independent', async () => {
    const timestamps = new SessionTimestamps(() => NOW);
    for (let i = 0; i < 500; i++) {
      await timestamps.next('0xa');
    }
    expect(await timestamps.next('0xb')).toBe(NOW);
  });

  it('waits instead of running more than a second ahead of the clock', async () => {
    let now = NOW;
    const timestamps = new SessionTimestamps(() => now);
    for (let i = 0; i <= SESSION_TIMESTAMP_MAX_AHEAD_MS; i++) {
      await timestamps.next('0xa');
    }
    const pending = timestamps.next('0xa');
    now = NOW + 1;
    expect(await pending).toBe(NOW + SESSION_TIMESTAMP_MAX_AHEAD_MS + 1);
  });

  it('never gives two waiting callers the same timestamp', async () => {
    let now = NOW;
    const timestamps = new SessionTimestamps(() => now);
    for (let i = 0; i <= SESSION_TIMESTAMP_MAX_AHEAD_MS; i++) {
      await timestamps.next('0xa');
    }
    const waiting = Array.from({ length: 50 }, () => timestamps.next('0xa'));
    const ticker = setInterval(() => {
      now += 1;
    }, 1);
    const issued = await Promise.all(waiting);
    clearInterval(ticker);
    expect(new Set(issued).size).toBe(issued.length);
    for (const timestamp of issued) {
      expect(timestamp).toBeGreaterThan(NOW + SESSION_TIMESTAMP_MAX_AHEAD_MS);
    }
  });

  it('jumps near the ahead limit after a replay rejection', async () => {
    const timestamps = new SessionTimestamps(() => NOW);
    expect(await timestamps.next('0xa')).toBe(NOW);
    const retried = await timestamps.next('0xa', true);
    expect(retried).toBeGreaterThan(NOW + 1_000 - 200 - 1);
    expect(retried).toBeLessThanOrEqual(NOW + 1_000);
    expect(await timestamps.next('0xb')).toBe(NOW);
  });
});

describe('WriteSequencer', () => {
  it('runs writes to one account one at a time and keeps going after a failure', async () => {
    const sequencer = new WriteSequencer();
    const order: string[] = [];
    let releaseFirst!: () => void;
    const first = sequencer.run('0xa', async () => {
      order.push('first start');
      await new Promise<void>((resolve) => (releaseFirst = resolve));
      order.push('first end');
      throw new Error('first failed');
    });
    const second = sequencer.run('0xa', async () => {
      order.push('second');
      return 2;
    });
    const otherAccount = sequencer.run('0xb', async () => {
      order.push('other account');
      return 3;
    });

    expect(await otherAccount).toBe(3);
    expect(order).toEqual(['first start', 'other account']);
    releaseFirst();
    await expect(first).rejects.toThrow('first failed');
    expect(await second).toBe(2);
    expect(order).toEqual(['first start', 'other account', 'first end', 'second']);
  });

  it('classifies only the four write operations as writes', () => {
    expect(
      Object.values(SessionOperation).filter((operation) => isSessionWrite(operation))
    ).toEqual([
      SessionOperation.PushDelta,
      SessionOperation.PushDeltaProposal,
      SessionOperation.SignDeltaProposal,
      SessionOperation.ExecuteDeltaProposal,
    ]);
  });
});
