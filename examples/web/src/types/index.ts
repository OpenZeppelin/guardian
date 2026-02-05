import type { SecretKey } from '@demox-labs/miden-sdk';
import type { SignatureScheme } from '@openzeppelin/miden-multisig-client';

export interface SignerKeyInfo {
  commitment: string;
  secretKey: SecretKey;
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
