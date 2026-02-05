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

export { FalconSigner, EcdsaSigner } from './signer.js';

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
