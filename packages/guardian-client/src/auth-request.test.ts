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

  it('tags the proposal check apart from proposal creation, which shares its body', () => {
    const request = { account_id: '0x11', nonce: 1, delta_payload: {} };
    const check = RequestAuthPayload.forOperation('CheckDeltaProposal', request);
    const creation = RequestAuthPayload.fromRequest(request);

    const tag = new TextEncoder().encode('guardian.CheckDeltaProposal');
    expect(Array.from(check.toBytes()).slice(0, tag.length + 1)).toEqual([...tag, 0]);
    expect(Array.from(check.toBytes())).not.toEqual(Array.from(creation.toBytes()));
  });
});
