import type { SignatureScheme } from '@openzeppelin/miden-multisig-client';

export type WalletSource = 'local' | 'miden-wallet';

export interface ExternalWalletState {
  source: WalletSource;
  connected: boolean;
  publicKey: string | null;
  commitment: string | null;
  scheme: SignatureScheme | null;
}
