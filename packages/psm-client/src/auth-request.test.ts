import { describe, expect, it } from 'vitest';
import { RequestAuthPayload } from './auth-request.js';

describe('RequestAuthPayload', () => {
  it('canonicalizes object keys recursively', () => {
    const left = RequestAuthPayload.fromRequest({ b: 2, a: { y: 2, x: 1 } });
    const right = RequestAuthPayload.fromRequest({ a: { x: 1, y: 2 }, b: 2 });

    expect(left.toCanonicalJson()).toEqual(right.toCanonicalJson());
  });
});
