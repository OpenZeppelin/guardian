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

/** Stable error identifiers for signer-scheme failures. */
export type SignerSchemeErrorCode = 'signer_scheme_mismatch';

/**
 * The acting signer's scheme differs from a scheme registered for the account's approvers or
 * its guardian, or from the scheme given for a signer being added.
 *
 * Raised before a transaction that rewrites the approver set or the guardian (or registers the
 * account with GUARDIAN) is built, because building it under the acting signer's scheme would
 * silently change the other schemes, and GUARDIAN binds a single scheme to an account: a
 * registration or guardian rotation under a scheme the account does not verify the guardian's
 * acknowledgements with would leave every transaction failing on-chain.
 */
export class SignerSchemeMismatchError extends Error {
  readonly code: SignerSchemeErrorCode = 'signer_scheme_mismatch';

  constructor(
    readonly signerScheme: SignatureScheme,
    readonly mismatched: SignerSpec[],
    /** The guardian, when its registered scheme is the one that differs. */
    readonly guardian?: SignerSpec,
  ) {
    super(
      `signer scheme '${signerScheme}' does not match the scheme of ` +
        [
          ...mismatched.map((signer) => `${signer.commitment} ('${signer.scheme}')`),
          ...(guardian ? [`the guardian ${guardian.commitment} ('${guardian.scheme}')`] : []),
        ].join(', ') +
        '; mixed-scheme accounts cannot be operated through GUARDIAN yet',
    );
    this.name = 'SignerSchemeMismatchError';
  }
}
