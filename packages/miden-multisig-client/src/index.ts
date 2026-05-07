/**
 * @openzeppelin/miden-multisig-client
 *
 * TypeScript SDK for Miden multisig accounts with Guardian integration.
 *
 * @example
 * ```typescript
 * import {
 *   MultisigClient,
 *   FalconSigner,
 * } from '@openzeppelin/miden-multisig-client';
 * import { MidenClient, AuthSecretKey } from '@miden-sdk/miden-sdk';
 *
 * const midenClient = await MidenClient.createDevnet();
 * const secretKey = AuthSecretKey.rpoFalconWithRNG(undefined);
 *
 * // Store in miden-sdk's keystore
 * await midenClient.keystore.insert(secretKey.publicKey(), secretKey);
 *
 * // Create a signer
 * const signer = new FalconSigner(secretKey);
 *
 * // Create multisig client
 * const client = new MultisigClient(midenClient, {
 *   guardianEndpoint: 'http://localhost:3000',
 *   midenRpcEndpoint: 'https://rpc.devnet.miden.io',
 * });
 *
 * // Get GUARDIAN pubkey for config
 * const guardianCommitment = await client.guardianClient.getPubkey();
 *
 * // Create multisig account
 * const config = { threshold: 2, signerCommitments: [signer.commitment, ...], guardianCommitment };
 * const multisig = await client.create(config, signer);
 *
 * // Register on GUARDIAN and work with proposals
 * await multisig.registerOnGuardian();
 * await multisig.syncProposals();
 * ```
 */

export {
  MultisigClient,
  type MultisigClientConfig,
  type RecoveredAccount,
} from './client.js';
export { lookupAuthDigest } from './lookupAuth.js';
export { Multisig, type AccountState } from './multisig.js';
export { AccountInspector, type DetectedMultisigConfig, type VaultBalance } from './inspector.js';
export {
  executeForSummary,
  buildUpdateSignersTransactionRequest,
  buildUpdateProcedureThresholdTransactionRequest,
  buildUpdateGuardianTransactionRequest,
  buildConsumeNotesTransactionRequest,
  buildP2idTransactionRequest,
} from './transaction.js';

export { GuardianHttpClient, GuardianHttpError } from '@openzeppelin/guardian-client';

export {
  FalconSigner,
  EcdsaSigner,
  ParaSigner,
  MidenWalletSigner,
  type ParaSigningContext,
  type WalletSigningContext,
} from './signer.js';
export { PublicKeyFormat } from './utils/key.js';
export { EcdsaFormat } from './utils/ecdsa.js';
export { tryComputeEcdsaCommitmentHex } from './utils/signature.js';

export {
  createMultisigAccount,
  validateMultisigConfig,
  buildMultisigStorageSlots,
  buildGuardianStorageSlots,
  storageLayoutBuilder,
  StorageLayoutBuilder,
} from './account/index.js';

export {
  PROCEDURE_ROOTS,
  getProcedureRoot,
  isProcedureName,
  getProcedureNames,
  type ProcedureName,
} from './procedures.js';

export type {
  // Account types
  MultisigAccountState,
  MultisigConfig,
  CreateAccountResult,
  ProcedureThreshold,

  // Proposal types
  Proposal,
  ProposalStatus,
  ProposalSignatureEntry,
  ProposalMetadata,
  ProposalType,
  ExportedProposal,
  ExportedTransactionProposal,
  SignTransactionProposalParams,
  TransactionProposal,
  TransactionProposalSignature,
  TransactionProposalStatus,

  // Transaction types
  TransactionType,

  // Note types
  ConsumableNote,
  NoteAsset,

  // Signature types
  FalconSignature,
  Signer,
  SignatureScheme,

  // GUARDIAN API types
  AuthConfig,
  DeltaObject,
  DeltaStatus,
  StateObject,
  CosignerSignature,

  // Request/Response types
  ConfigureRequest,
  ConfigureResponse,
  DeltaProposalRequest,
  DeltaProposalResponse,
  ProposalsResponse,
  PubkeyResponse,
  SignProposalRequest,
} from './types.js';
