import type { AuthConfig } from '@openzeppelin/guardian-client';
import type { MultisigConfig, SignatureScheme, SignerInput, SignerSpec } from '../types.js';
import { normalizeHexWord } from '../utils/encoding.js';

export const DEFAULT_SIGNATURE_SCHEME: SignatureScheme = 'falcon';

/**
 * Resolves every approver of a configuration to a {@link SignerSpec}, in storage order.
 *
 * A bare commitment takes `signatureScheme` (default `'falcon'`); a spec keeps its own scheme.
 * This is the single place that rule lives: account creation, the `approver_schemes` storage
 * layout and the update-signers advice all read approvers through it.
 */
export function resolveSignerSpecs(
  config: Pick<MultisigConfig, 'signerCommitments' | 'signatureScheme'>,
): SignerSpec[] {
  const defaultScheme = assertSignatureScheme(config.signatureScheme ?? DEFAULT_SIGNATURE_SCHEME);
  return config.signerCommitments.map((signer) => toSignerSpec(signer, defaultScheme));
}

/** Commitments of the given approvers, in order. */
export function signerCommitmentsOf(signers: readonly SignerInput[]): string[] {
  return signers.map((signer) => (typeof signer === 'string' ? signer : signer.commitment));
}

/** Whether every approver uses `scheme`. */
export function allSignersUse(signers: readonly SignerSpec[], scheme: SignatureScheme): boolean {
  return signers.every((signer) => signer.scheme === scheme);
}

function toSignerSpec(signer: SignerInput, defaultScheme: SignatureScheme): SignerSpec {
  if (typeof signer === 'string') {
    return { commitment: normalizeCommitmentPrefix(signer), scheme: defaultScheme };
  }
  return {
    commitment: normalizeCommitmentPrefix(signer.commitment),
    scheme: assertSignatureScheme(signer.scheme),
  };
}

/**
 * Gives a commitment the `0x` prefix `Word.fromHex` accepts: an `0X` prefix is lowered and a
 * missing one added. The digits are left as given.
 */
function normalizeCommitmentPrefix(commitment: string): string {
  const digits =
    commitment.startsWith('0x') || commitment.startsWith('0X') ? commitment.slice(2) : commitment;
  return `0x${digits}`;
}

function assertSignatureScheme(scheme: SignatureScheme): SignatureScheme {
  switch (scheme) {
    case 'falcon':
    case 'ecdsa':
      return scheme;
    default: {
      const unknownScheme: never = scheme;
      throw new Error(`unsupported signature scheme: ${String(unknownScheme)}`);
    }
  }
}

/**
 * The auth GUARDIAN registers an account with these approvers and this GUARDIAN scheme under.
 * Approvers that all use the GUARDIAN's scheme get that scheme's single-scheme variant, which
 * servers before per-signer schemes also accept; any other set gets the per-signer `Miden`
 * variant (issue #563).
 */
export function guardianAuthConfig(
  signers: readonly SignerSpec[],
  guardianScheme: SignatureScheme,
): AuthConfig {
  if (allSignersUse(signers, guardianScheme)) {
    const cosigner_commitments = signers.map((signer) => signer.commitment);
    return guardianScheme === 'ecdsa'
      ? { MidenEcdsa: { cosigner_commitments } }
      : { MidenFalconRpo: { cosigner_commitments } };
  }
  return {
    Miden: {
      cosigners: signers.map(({ commitment, scheme }) => ({ commitment, scheme })),
      guardian_scheme: guardianScheme,
    },
  };
}

/**
 * The `signerSchemes` an update-signers proposal carries for `targets` on an account whose
 * approvers are `current`: none when every target signer uses the proposer's scheme and
 * {@link resolveTargetSigners} reads the same targets back without names, as it reads a
 * proposal from before per-signer schemes; one per target signer otherwise. Single-scheme
 * proposals keep their wire shape. It is the Rust SDK's rule for per-signer schemes (#549), so
 * each SDK rebuilds the other's proposals once both have it.
 */
export function targetSignerSchemes(
  targets: readonly SignerSpec[],
  proposerScheme: SignatureScheme,
  current: readonly SignerSpec[],
): SignatureScheme[] | undefined {
  if (allSignersUse(targets, proposerScheme)) {
    try {
      const read = resolveTargetSigners(signerCommitmentsOf(targets), undefined, current);
      if (read.every((signer, index) => signer.scheme === targets[index].scheme)) {
        return undefined;
      }
    } catch {
      // Unreadable without names: name them.
    }
  }
  return targets.map((target) => target.scheme);
}

/**
 * The approver set an update-signers proposal writes, given its target `commitments`, the
 * `schemes` it names for them (`signerSchemes` as GUARDIAN or an exported proposal serves it,
 * so untrusted) and the account's `current` approvers.
 *
 * A proposal that names no schemes gives every target signer the scheme the account stores
 * for the first approver it keeps (the account's first approver when it keeps none), which is
 * what its proposer wrote whenever it rewrote no scheme. Either way a target signer the
 * account already stores under another scheme is refused with
 * {@link SignerSchemeMismatchError}.
 */
export function resolveTargetSigners(
  commitments: readonly string[],
  schemes: readonly string[] | undefined,
  current: readonly SignerSpec[],
): SignerSpec[] {
  let resolved: SignatureScheme[];
  if (schemes === undefined || (Array.isArray(schemes) && schemes.length === 0)) {
    const kept = keptScheme(commitments, current);
    resolved = commitments.map(() => kept);
  } else if (!Array.isArray(schemes)) {
    throw new Error('proposal metadata.signerSchemes must be an array of scheme names');
  } else if (schemes.length !== commitments.length) {
    throw new Error(
      `proposal metadata names ${schemes.length} signerSchemes for ${commitments.length} signer commitments`,
    );
  } else {
    resolved = schemes.map(parseSchemeName);
  }
  const targets = commitments.map((commitment, index) => ({ commitment, scheme: resolved[index] }));
  const changes = targets.flatMap((target): SignerSchemeChange[] => {
    const stored = storedSchemeOf(current, target.commitment);
    return stored && stored !== target.scheme
      ? [{ commitment: target.commitment, stored, proposed: target.scheme }]
      : [];
  });
  if (changes.length > 0) {
    throw new SignerSchemeMismatchError(changes);
  }
  return targets;
}

function keptScheme(commitments: readonly string[], current: readonly SignerSpec[]): SignatureScheme {
  const approver =
    current.find((signer) => commitments.some((c) => sameCommitment(c, signer.commitment))) ??
    current[0];
  if (!approver) {
    throw new Error('the account has no approvers');
  }
  return approver.scheme;
}

/** A scheme name as the Rust SDK and GUARDIAN parse it: `falcon` or `ecdsa`, in any case. */
function parseSchemeName(name: unknown): SignatureScheme {
  const lowered = typeof name === 'string' ? name.toLowerCase() : undefined;
  if (lowered === 'falcon' || lowered === 'ecdsa') {
    return lowered;
  }
  throw new Error(`proposal metadata.signerSchemes: unsupported signature scheme '${String(name)}'`);
}

function sameCommitment(a: string, b: string): boolean {
  return normalizeHexWord(a) === normalizeHexWord(b);
}

/** The scheme `signers` stores for `commitment`, whatever its hex case or prefix. */
export function storedSchemeOf(
  signers: readonly SignerSpec[],
  commitment: string,
): SignatureScheme | undefined {
  return signers.find((signer) => sameCommitment(signer.commitment, commitment))?.scheme;
}

/** Stable error identifiers for signer-scheme failures. */
export type SignerSchemeErrorCode =
  | 'signer_scheme_mismatch'
  | 'key_scheme_mismatch'
  | 'mixed_schemes_unsupported';

/** An approver a signer update would write under another scheme than the account stores. */
export interface SignerSchemeChange {
  commitment: string;
  stored: SignatureScheme;
  proposed: SignatureScheme;
}

/**
 * A signer update would write an approver under another signature scheme than the one the
 * account stores for it. The approver's key verifies only under its own scheme, so the update
 * would lock it out.
 *
 * Raised for a proposal that names such a scheme, and for one that names none (any from an SDK
 * before per-signer schemes, which wrote its own scheme for every approver) on an account whose
 * kept approvers do not share one, before it is signed or executed. The Rust SDK's counterpart
 * with per-signer schemes (#549) is `MultisigError::SignerSchemeMismatch`.
 */
export class SignerSchemeMismatchError extends Error {
  readonly code: SignerSchemeErrorCode = 'signer_scheme_mismatch';

  constructor(readonly changes: SignerSchemeChange[]) {
    super(
      'signer update would change the signature scheme of ' +
        changes
          .map(
            ({ commitment, stored, proposed }) =>
              `${commitment} (stored '${stored}', proposed '${proposed}')`,
          )
          .join(', ') +
        "; an approver's key verifies only under its own scheme",
    );
    this.name = 'SignerSchemeMismatchError';
  }
}

/**
 * The account stores this client's key under another signature scheme than the key's own. Its
 * approvals could not verify on chain, and GUARDIAN refuses them, so the client neither
 * proposes nor signs. The Rust SDK's counterpart with per-signer schemes (#549) is
 * `MultisigError::KeySchemeMismatch`.
 */
export class KeySchemeMismatchError extends Error {
  readonly code: SignerSchemeErrorCode = 'key_scheme_mismatch';

  constructor(
    readonly commitment: string,
    readonly keyScheme: SignatureScheme,
    readonly storedScheme: SignatureScheme,
  ) {
    super(
      `this client's ${keyScheme} key ${commitment} is stored on the account under ` +
        `'${storedScheme}'; its approvals cannot verify`,
    );
    this.name = 'KeySchemeMismatchError';
  }
}

/**
 * GUARDIAN refused the per-signer `Miden` auth an account whose approvers and GUARDIAN do not
 * all share one scheme registers with. Servers up to 0.18.3 know only the single-scheme
 * variants; register such an account on a server with per-signer schemes (issue #563).
 */
export class GuardianMixedSchemesUnsupportedError extends Error {
  readonly code: SignerSchemeErrorCode = 'mixed_schemes_unsupported';

  constructor(options?: { cause?: unknown }) {
    super(
      'this GUARDIAN does not accept accounts whose approvers and GUARDIAN do not all share ' +
        'one signature scheme (servers up to 0.18.3); register the account on a server with ' +
        'per-signer schemes',
      options,
    );
    this.name = 'GuardianMixedSchemesUnsupportedError';
  }
}
