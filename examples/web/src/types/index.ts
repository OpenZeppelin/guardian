import type { AuthSecretKey } from '@miden-sdk/miden-sdk';
import type { SignatureScheme } from '@openzeppelin/miden-multisig-client';

export interface LocalSignerInfo {
  commitment: string;
  secretKey: AuthSecretKey;
}

// This tab's local signer info for both supported schemes.
export interface SignerInfo {
  falcon: LocalSignerInfo;
  ecdsa: LocalSignerInfo;
  activeScheme: SignatureScheme;
}

// Other signers (from other tabs)
export interface OtherSigner {
  id: string;
  commitment: string;
}
