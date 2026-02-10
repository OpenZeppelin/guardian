import type { ProposalSignature, Signer } from '@openzeppelin/psm-client';
import type { SignatureScheme } from '../types.js';

export function toPsmSignature(
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

export async function buildPsmSignatureFromSigner(
  signer: Signer,
  commitment: string,
): Promise<ProposalSignature> {
  const signatureHex = await signer.signCommitment(commitment);
  return toPsmSignature(signer.scheme, signatureHex, signer.publicKey);
}
