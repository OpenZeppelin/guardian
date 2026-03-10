import type { ProposalSignatureEntry } from '../types.js';
import { canonicalizeSignature, normalizeSignerCommitment } from '../utils/signature.js';

export class ProposalSignatures {
  private readonly signatures: ProposalSignatureEntry[];

  constructor(
    signatures: ProposalSignatureEntry[],
    signerCommitments: string[],
    context: string,
  ) {
    const expectedSigners = new Set<string>();
    for (const signerCommitment of signerCommitments) {
      let normalizedCommitment: string;
      try {
        normalizedCommitment = normalizeSignerCommitment(signerCommitment);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        throw new Error(`${context}: ${message}`);
      }
      expectedSigners.add(normalizedCommitment);
    }

    const signaturesBySigner = new Map<string, ProposalSignatureEntry>();
    for (const signature of signatures) {
      let canonicalized: ProposalSignatureEntry;
      try {
        canonicalized = canonicalizeSignature(signature, expectedSigners);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        throw new Error(`${context}: ${message}`);
      }

      if (signaturesBySigner.has(canonicalized.signerId)) {
        throw new Error(`${context}: duplicate signatures for signer ${canonicalized.signerId}`);
      }

      signaturesBySigner.set(canonicalized.signerId, canonicalized);
    }

    this.signatures = Array.from(signaturesBySigner.values());
  }

  entries(): ProposalSignatureEntry[] {
    return [...this.signatures];
  }

  count(): number {
    return this.signatures.length;
  }

  hasSigner(signerId: string): boolean {
    const normalizedSigner = normalizeSignerCommitment(signerId);
    return this.signatures.some((signature) => signature.signerId === normalizedSigner);
  }

  static mergeEntries(entryGroups: ProposalSignatureEntry[][]): ProposalSignatureEntry[] {
    const signaturesBySigner = new Map<string, ProposalSignatureEntry>();

    for (const group of entryGroups) {
      for (const signature of group) {
        signaturesBySigner.set(signature.signerId, signature);
      }
    }

    return Array.from(signaturesBySigner.values());
  }
}
