import type { AuthSecretKey } from '@miden-sdk/miden-sdk';

// This tab's signer info
export interface SignerInfo {
  commitment: string;
  secretKey: AuthSecretKey;
}

// Other signers (from other tabs)
export interface OtherSigner {
  id: string;
  commitment: string;
}
