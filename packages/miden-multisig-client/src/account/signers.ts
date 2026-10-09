import type { MultisigConfig, SignatureScheme, SignerInput, SignerSpec } from '../types.js';

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
    return { commitment: signer, scheme: defaultScheme };
  }
  return { commitment: signer.commitment, scheme: assertSignatureScheme(signer.scheme) };
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

/** Stable error identifiers for signer-scheme failures. */
export type SignerSchemeErrorCode = 'signer_scheme_mismatch';

/**
 * The acting signer's scheme differs from a scheme registered for the account's approvers.
 *
 * Raised before a transaction that rewrites the approver set (or registers the account with
 * GUARDIAN) is built, because building it under the acting signer's scheme would silently change
 * the other approvers' schemes, and GUARDIAN binds a single scheme to an account.
 */
export class SignerSchemeMismatchError extends Error {
  readonly code: SignerSchemeErrorCode = 'signer_scheme_mismatch';

  constructor(
    readonly signerScheme: SignatureScheme,
    readonly mismatched: SignerSpec[],
  ) {
    super(
      `signer scheme '${signerScheme}' does not match the registered scheme of ` +
        mismatched.map((signer) => `${signer.commitment} ('${signer.scheme}')`).join(', ') +
        '; mixed-scheme approver sets cannot be operated through GUARDIAN yet',
    );
    this.name = 'SignerSchemeMismatchError';
  }
}
