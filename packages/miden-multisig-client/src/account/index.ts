/**
 * Account creation and management utilities.
 */

export {
  createMultisigAccount,
  validateMultisigConfig,
} from './builder.js';

export {
  buildMultisigStorageSlots,
  buildGuardianStorageSlots,
  storageLayoutBuilder,
  StorageLayoutBuilder,
} from './storage.js';

export {
  resolveSignerSpecs,
  SignerSchemeMismatchError,
  KeySchemeMismatchError,
  GuardianMixedSchemesUnsupportedError,
  type SignerSchemeChange,
  type SignerSchemeErrorCode,
} from './signers.js';
