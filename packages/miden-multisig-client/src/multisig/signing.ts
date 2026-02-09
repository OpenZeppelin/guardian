import type { ProposalSignature, Signer } from '@openzeppelin/psm-client';
import type { SignatureScheme } from '../types.js';

export function buildServerSignatureFromSigner(
  signer: Signer,
  commitment: string,
): ProposalSignature {
  const signatureHex = signer.signCommitment(commitment);
  if (signer.scheme === 'ecdsa') {
    return {
      scheme: 'ecdsa',
      signature: signatureHex,
      publicKey: signer.publicKey,
    };
  }
  return { scheme: 'falcon', signature: signatureHex };
}

export function buildServerSignatureExternal(
  scheme: SignatureScheme,
  signatureHex: string,
  publicKey?: string,
): ProposalSignature {
  if (scheme === 'ecdsa') {
    if (!publicKey) {
      throw new Error('ECDSA external signature requires publicKey');
    }
    return {
      scheme: 'ecdsa',
      signature: signatureHex,
      publicKey,
    };
  }
  return { scheme: 'falcon', signature: signatureHex };
}
