import { describe, expect, it } from 'vitest';
import { RequestAuthPayload } from './auth-request.js';

describe('RequestAuthPayload', () => {
  it('canonicalizes object keys recursively', () => {
    const left = RequestAuthPayload.fromRequest({ b: 2, a: { y: 2, x: 1 } });
    const right = RequestAuthPayload.fromRequest({ a: { x: 1, y: 2 }, b: 2 });

    expect(left.toCanonicalJson()).toEqual(right.toCanonicalJson());
  });

  it('prefixes an operation payload with its tag and a zero byte, and keeps the canonical JSON', () => {
    const request = { proposal_id: '0x22', account_id: '0x11' };
    const plain = RequestAuthPayload.fromRequest(request);
    const tagged = RequestAuthPayload.forOperation('ExecuteDeltaProposal', request);

    const tag = new TextEncoder().encode('guardian.ExecuteDeltaProposal');
    const json = new TextEncoder().encode('{"account_id":"0x11","proposal_id":"0x22"}');
    expect(Array.from(tagged.toBytes())).toEqual([...tag, 0, ...json]);
    expect(Array.from(plain.toBytes())).toEqual([...json]);
    expect(tagged.toCanonicalJson()).toBe(plain.toCanonicalJson());
  });
});
