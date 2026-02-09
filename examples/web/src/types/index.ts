import type { AuthSecretKey } from '@miden-sdk/miden-sdk';
import type { SignatureScheme } from '@openzeppelin/miden-multisig-client';

export interface SignerKeyInfo {
  commitment: string;
  secretKey: AuthSecretKey;
}

// This tab's signer info
export interface SignerInfo {
  falcon: SignerKeyInfo;
  ecdsa: SignerKeyInfo;
  activeScheme: SignatureScheme;
}

// Other signers (from other tabs)
export interface OtherSigner {
  id: string;
  commitment: string;
}
