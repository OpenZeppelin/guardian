import { describe, expect, it } from 'vitest';

import type { SignatureScheme, SignerSpec } from '../types.js';
import {
  allSignersUse,
  guardianAuthConfig,
  KeySchemeMismatchError,
  resolveSignerSpecs,
  resolveTargetSigners,
  signerCommitmentsOf,
  SignerSchemeMismatchError,
  targetSignerSchemes,
} from './signers.js';

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

  it('normalizes an uppercase or missing 0x prefix on bare and spec commitments', () => {
    expect(
      resolveSignerSpecs({
        signerCommitments: ['0X' + 'a'.repeat(64), { commitment: 'b'.repeat(64), scheme: 'ecdsa' }],
      }),
    ).toEqual([
      { commitment: A, scheme: 'falcon' },
      { commitment: B, scheme: 'ecdsa' },
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

const F: SignatureScheme = 'falcon';
const E: SignatureScheme = 'ecdsa';

/** Approver `v` as a 32-byte commitment. */
const word = (v: number) => '0x' + v.toString(16).padStart(64, '0');
const specs = (entries: Array<[number, SignatureScheme]>): SignerSpec[] =>
  entries.map(([v, scheme]) => ({ commitment: word(v), scheme }));
const words = (values: number[]) => values.map(word);

describe('guardianAuthConfig', () => {
  it('registers uniform approvers under the single-scheme variant', () => {
    expect(guardianAuthConfig(specs([[1, F], [2, F]]), F)).toEqual({
      MidenFalconRpo: { cosigner_commitments: words([1, 2]) },
    });
    expect(guardianAuthConfig(specs([[3, E]]), E)).toEqual({
      MidenEcdsa: { cosigner_commitments: words([3]) },
    });
  });

  it('registers mixed approvers under the per-signer variant', () => {
    expect(guardianAuthConfig(specs([[1, F], [2, E]]), E)).toEqual({
      Miden: {
        cosigners: [
          { commitment: word(1), scheme: 'falcon' },
          { commitment: word(2), scheme: 'ecdsa' },
        ],
        guardian_scheme: 'ecdsa',
      },
    });
  });

  // The single-scheme variants name one scheme for the approvers and the GUARDIAN.
  it('registers approvers of another scheme than the GUARDIAN per signer', () => {
    expect(guardianAuthConfig(specs([[1, F]]), E)).toHaveProperty('Miden');
  });
});

describe('targetSignerSchemes / resolveTargetSigners (shared with the Rust SDK)', () => {
  it('omits the schemes only when every target uses the proposer scheme', () => {
    const current = specs([[1, F], [2, F]]);
    expect(targetSignerSchemes(specs([[1, F], [2, F], [3, F]]), F, current)).toBeUndefined();
    expect(targetSignerSchemes(specs([[1, F], [2, F], [3, E]]), F, current)).toEqual([F, F, E]);
    // A uniform target the proposer does not share still names its scheme:
    // without it a reader would assume the kept approver's.
    expect(targetSignerSchemes(specs([[1, F]]), E, specs([[1, F], [2, E]]))).toEqual([F]);
  });

  // A target that keeps no approver reads back without names under the first
  // approver's scheme, so one of another scheme is named even when it is the
  // proposer's.
  it('names a replacement the reader would misread', () => {
    expect(targetSignerSchemes(specs([[3, E], [4, E]]), E, specs([[1, F], [2, E]]))).toEqual([E, E]);
  });

  // Whatever the writer names or omits, the reader resolves exactly the
  // targets written, for every target that keeps the stored schemes of the
  // approvers it keeps. Same matrix as the Rust SDK's
  // `every_written_target_reads_back_exactly`.
  it('reads back every written target exactly', () => {
    const accounts = [
      specs([[1, F]]),
      specs([[1, E]]),
      specs([[1, F], [2, E]]),
      specs([[1, E], [2, F]]),
      specs([[1, F], [2, E], [3, F]]),
      specs([[1, E], [2, E], [3, F]]),
    ];
    let cases = 0;
    for (const current of accounts) {
      for (let keptMask = 0; keptMask < 1 << current.length; keptMask += 1) {
        const kept = current.filter((_, index) => (keptMask & (1 << index)) !== 0);
        for (const added of [undefined, F, E]) {
          const targets = added ? [...kept, { commitment: word(9), scheme: added }] : kept;
          if (targets.length === 0) {
            continue;
          }
          for (const proposer of [F, E]) {
            const names = targetSignerSchemes(targets, proposer, current);
            const read = resolveTargetSigners(signerCommitmentsOf(targets), names, current);
            expect(read, JSON.stringify({ current, proposer, names })).toEqual(targets);
            cases += 1;
          }
        }
      }
    }
    expect(cases).toBeGreaterThan(100);
  });

  // A proposal from before per-signer schemes on a single-scheme account: every
  // target signer, the added one too, takes the account's scheme.
  it('gives unnamed targets the scheme of the kept approvers', () => {
    expect(resolveTargetSigners(words([1, 2, 3]), undefined, specs([[1, E], [2, E]]))).toEqual(
      specs([[1, E], [2, E], [3, E]]),
    );
    expect(resolveTargetSigners(words([1, 2, 3]), [], specs([[1, E], [2, E]]))).toEqual(
      specs([[1, E], [2, E], [3, E]]),
    );
  });

  // Removing the only approver of the other scheme leaves a target that shares
  // one scheme, which needs no names.
  it('keeps the stored scheme after removing the approver of the other scheme', () => {
    expect(resolveTargetSigners(words([1, 3]), undefined, specs([[1, F], [2, E], [3, F]]))).toEqual(
      specs([[1, F], [3, F]]),
    );
  });

  it('reads unnamed schemes from a kept approver, not the first one', () => {
    expect(resolveTargetSigners(words([2]), undefined, specs([[1, F], [2, E]]))).toEqual(
      specs([[2, E]]),
    );
  });

  it('matches kept approvers whatever the commitment case or prefix', () => {
    const upper = '0X' + word(2).slice(2).toUpperCase();
    expect(resolveTargetSigners([upper], undefined, specs([[1, F], [2, E]]))).toEqual([
      { commitment: upper, scheme: E },
    ]);
  });

  // Issue #549: an older SDK stamped its own scheme on every approver, so its
  // proposal on a mixed account rewrites the others.
  it('refuses unnamed schemes over kept approvers of both schemes', () => {
    let error: unknown;
    try {
      resolveTargetSigners(words([1, 2, 3]), undefined, specs([[1, F], [2, E]]));
    } catch (e) {
      error = e;
    }
    expect(error).toBeInstanceOf(SignerSchemeMismatchError);
    expect(error).toMatchObject({
      code: 'signer_scheme_mismatch',
      changes: [{ commitment: word(2), stored: E, proposed: F }],
    });
    expect((error as Error).message).toContain(`${word(2)} (stored 'ecdsa', proposed 'falcon')`);
  });

  it('takes named schemes as given', () => {
    expect(resolveTargetSigners(words([1, 2, 3]), [F, E, E], specs([[1, F], [2, E]]))).toEqual(
      specs([[1, F], [2, E], [3, E]]),
    );
  });

  it('refuses named schemes that rewrite a kept approver', () => {
    expect(() => resolveTargetSigners(words([1, 2]), [F, F], specs([[1, F], [2, E]]))).toThrow(
      SignerSchemeMismatchError,
    );
  });

  it('refuses named schemes that do not align with the commitments', () => {
    expect(() => resolveTargetSigners(words([1, 2]), [F], specs([[1, F]]))).toThrow(
      /names 1 signerSchemes for 2 signer commitments/,
    );
  });

  it('refuses names it does not know, and a value that is not a list', () => {
    expect(() => resolveTargetSigners(words([1]), ['rsa'], specs([[1, F]]))).toThrow(
      /unsupported signature scheme 'rsa'/,
    );
    expect(() =>
      resolveTargetSigners(words([1]), 'falcon' as unknown as string[], specs([[1, F]])),
    ).toThrow(/must be an array/);
  });

  it('parses names in any case, as GUARDIAN and the Rust SDK do', () => {
    expect(resolveTargetSigners(words([1, 2]), ['FALCON', 'Ecdsa'], specs([[1, F], [2, E]]))).toEqual(
      specs([[1, F], [2, E]]),
    );
  });
});

describe('KeySchemeMismatchError', () => {
  it('names the key, its scheme and the stored one', () => {
    const error = new KeySchemeMismatchError(word(1), F, E);
    expect(error).toMatchObject({
      code: 'key_scheme_mismatch',
      commitment: word(1),
      keyScheme: F,
      storedScheme: E,
    });
    expect(error.message).toBe(
      `this client's falcon key ${word(1)} is stored on the account under 'ecdsa'; its approvals cannot verify`,
    );
  });
});
