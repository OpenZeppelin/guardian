import type { ProposalSignature, Signer } from '@openzeppelin/guardian-client';
import type { SignatureScheme } from '../types.js';

export function toGuardianSignature(
  scheme: SignatureScheme,
  signatureHex: string,
  publicKey?: string,
): ProposalSignature {
  if (scheme === 'ecdsa') {
    if (!publicKey) {
      throw new Error('ECDSA signature requires publicKey');
    }
    return { scheme: 'ecdsa', signature: signatureHex, publicKey };
  }
  return { scheme: 'falcon', signature: signatureHex };
}

export async function buildGuardianSignatureFromSigner(
  signer: Signer,
  commitment: string,
): Promise<ProposalSignature> {
  const signatureHex = await signer.signCommitment(commitment);
  return toGuardianSignature(signer.scheme, signatureHex, signer.publicKey);
}
