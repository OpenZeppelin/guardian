export { MultisigClient, type MultisigClientConfig } from './client.js';
export { Multisig, type AccountState } from './multisig.js';
export { AccountInspector, type DetectedMultisigConfig, type VaultBalance } from './inspector.js';
export {
  executeForSummary,
  buildUpdateSignersTransactionRequest,
  buildUpdatePsmTransactionRequest,
  buildConsumeNotesTransactionRequest,
  buildP2idTransactionRequest,
} from './transaction.js';

export { PsmHttpClient, PsmHttpError } from '@openzeppelin/psm-client';

export { FalconSigner, EcdsaSigner, ParaSigner, MidenWalletSigner } from './signers/index.js';
export type { ParaSigningContext } from './signers/para.js';
export type { WalletSigningContext } from './signers/miden-wallet.js';
export { AuthDigest } from './utils/digest.js';
export { EcdsaFormat } from './utils/ecdsa.js';
export { PublicKeyFormat } from './utils/key.js';
export { wordToBytes } from './utils/word.js';
export { tryComputeEcdsaCommitmentHex, tryComputeCommitmentHex } from './utils/signature.js';

export {
  createMultisigAccount,
  validateMultisigConfig,
  buildMultisigStorageSlots,
  buildPsmStorageSlots,
  storageLayoutBuilder,
  StorageLayoutBuilder,
} from './account/index.js';

export {
  PROCEDURE_ROOTS,
  PROCEDURE_ROOTS_FALCON,
  PROCEDURE_ROOTS_ECDSA,
  getProcedureRoot,
  isProcedureName,
  getProcedureNames,
  type ProcedureName,
} from './procedures.js';

export type {
  MultisigAccountState,
  MultisigConfig,
  CreateAccountResult,
  ProcedureThreshold,
  TransactionProposal,
  TransactionProposalStatus,
  TransactionProposalSignature,
  ExportedTransactionProposal,
  SignTransactionProposalParams,
  SyncResult,
  TransactionProposalResult,
  ProposalMetadata,
  ProposalType,
  TransactionType,
  ConsumableNote,
  NoteAsset,
  FalconSignature,
  EcdsaSignature,
  ProposalSignature,
  SignatureScheme,
  Signer,
  AuthConfig,
  DeltaObject,
  DeltaStatus,
  StateObject,
  CosignerSignature,
  ConfigureRequest,
  ConfigureResponse,
  DeltaProposalRequest,
  DeltaProposalResponse,
  ProposalsResponse,
  PubkeyResponse,
  SignProposalRequest,
} from './types.js';
