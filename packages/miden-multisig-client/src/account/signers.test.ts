import { describe, expect, it } from 'vitest';

import type { SignatureScheme } from '../types.js';
import { allSignersUse, resolveSignerSpecs, signerCommitmentsOf } from './signers.js';

const A = '0x' + 'a'.repeat(64);
const B = '0x' + 'b'.repeat(64);

describe('resolveSignerSpecs', () => {
  it('gives bare commitments the Falcon default when no scheme is configured', () => {
    expect(resolveSignerSpecs({ signerCommitments: [A, B] })).toEqual([
      { commitment: A, scheme: 'falcon' },
      { commitment: B, scheme: 'falcon' },
    ]);
  });

  it('gives bare commitments the configured signatureScheme', () => {
    expect(resolveSignerSpecs({ signerCommitments: [A], signatureScheme: 'ecdsa' })).toEqual([
      { commitment: A, scheme: 'ecdsa' },
    ]);
  });

  it('keeps each spec scheme and the order of a mixed list', () => {
    expect(
      resolveSignerSpecs({
        signerCommitments: [{ commitment: A, scheme: 'ecdsa' }, B],
        signatureScheme: 'falcon',
      }),
    ).toEqual([
      { commitment: A, scheme: 'ecdsa' },
      { commitment: B, scheme: 'falcon' },
    ]);
  });

  it('rejects an unknown approver scheme', () => {
    expect(() =>
      resolveSignerSpecs({
        signerCommitments: [{ commitment: A, scheme: 'rsa' as SignatureScheme }],
      }),
    ).toThrow(/unsupported signature scheme: rsa/);
  });

  it('rejects an unknown default scheme', () => {
    expect(() =>
      resolveSignerSpecs({ signerCommitments: [A], signatureScheme: 'rsa' as SignatureScheme }),
    ).toThrow(/unsupported signature scheme: rsa/);
  });
});

describe('signerCommitmentsOf', () => {
  it('returns the commitments of bare and spec approvers in order', () => {
    expect(signerCommitmentsOf([A, { commitment: B, scheme: 'ecdsa' }])).toEqual([A, B]);
  });
});

describe('allSignersUse', () => {
  it('is true only when every approver uses the scheme', () => {
    const mixed = resolveSignerSpecs({ signerCommitments: [A, { commitment: B, scheme: 'ecdsa' }] });

    expect(allSignersUse(mixed, 'falcon')).toBe(false);
    expect(allSignersUse(mixed.slice(0, 1), 'falcon')).toBe(true);
  });
});
