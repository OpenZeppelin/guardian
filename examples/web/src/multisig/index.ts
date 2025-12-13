/**
 * Multisig module for creating and managing Miden multisig accounts.
 */

export { createMultisigAccount, type MidenSdkTypes } from './account-builder';
export { loadMultisigMasm, loadPsmMasm } from './masm-loader';
export type { MultisigConfig, CreateAccountResult } from './types';

// Keystore exports
export {
  generateKey,
  loadKeys,
  deleteKey,
  getKey,
  renameKey,
  clearKeystore,
  loadSecretKey,
  type KeyEntry,
  type KeystoreSdkTypes,
} from './keystore';

// Signer exports
export { createSigner, type SignerSdkTypes } from './signer';
