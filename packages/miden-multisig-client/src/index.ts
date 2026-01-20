/**
 * @openzeppelin/miden-multisig-client
 *
 * TypeScript SDK for Miden multisig accounts with PSM (Private State Manager) integration.
 *
 * @example
 * ```typescript
 * import {
 *   MultisigClient,
 *   FalconSigner,
 * } from '@openzeppelin/miden-multisig-client';
 * import { WebClient, SecretKey } from '@demox-labs/miden-sdk';
 *
 * // Initialize WebClient
 * const webClient = await WebClient.createClient('https://rpc.testnet.miden.io:443');
 * await webClient.syncState();
 *
 * // Generate a key dynamically
 * const seed = new Uint8Array(32);
 * crypto.getRandomValues(seed);
 * const secretKey = SecretKey.rpoFalconWithRNG(seed);
 *
 * // Store in miden-sdk's keystore
 * await webClient.addAccountSecretKeyToWebStore(secretKey);
 *
 * // Create a signer
 * const signer = new FalconSigner(secretKey);
 *
 * // Create multisig client
 * const client = new MultisigClient(webClient, { psmEndpoint: 'http://localhost:3000' });
 *
 * // Get PSM pubkey for config
 * const psmCommitment = await client.psmClient.getPubkey();
 *
 * // Create multisig account
 * const config = { threshold: 2, signerCommitments: [signer.commitment, ...], psmCommitment };
 * const multisig = await client.create(config, signer);
 *
 * // Register on PSM and work with proposals
 * await multisig.registerOnPsm();
 * await multisig.syncProposals();
 * ```
 */

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

export { FalconSigner } from './signer.js';

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

  // Transaction types
  TransactionType,

  // Note types
  ConsumableNote,
  NoteAsset,

  // Signature types
  FalconSignature,
  Signer,

  // PSM API types
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
