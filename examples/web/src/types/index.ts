import type { SecretKey } from '@demox-labs/miden-sdk';

// This tab's signer info
export interface SignerInfo {
  commitment: string;
  secretKey: SecretKey;
}

// Other signers (from other tabs)
export interface OtherSigner {
  id: string;
  commitment: string;
}
