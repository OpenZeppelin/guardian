/**
 * @openzeppelin/miden-multisig-client
 *
 * TypeScript SDK for Miden multisig accounts with PSM (Private State Manager) integration.
 * This provides the same functionality as the Rust `miden-multisig-client` crate
 * but for browser and Node.js environments.
 *
 * @example
 * ```typescript
 * import {
 *   MultisigClient,
 *   FalconSigner,
 *   setMasmBaseUrl,
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

// =============================================================================
// Client Classes
// =============================================================================

export { MultisigClient, type MultisigClientConfig } from './client.js';
export { Multisig, type AccountState } from './multisig.js';
export { AccountInspector, type DetectedMultisigConfig } from './inspector.js';
export { executeForSummary, buildUpdateSignersTransactionRequest } from './transaction.js';

// =============================================================================
// PSM Client (re-exported from @openzeppelin/psm-client)
// =============================================================================

export { PsmHttpClient, PsmHttpError } from '@openzeppelin/psm-client';

// =============================================================================
// Signer
// =============================================================================

export { FalconSigner } from './signer.js';

// =============================================================================
// Account Creation
// =============================================================================

export {
  createMultisigAccount,
  validateMultisigConfig,
  buildMultisigStorageSlots,
  buildPsmStorageSlots,
  loadMasmFile,
  loadMultisigMasm,
  loadPsmMasm,
  getMultisigMasm,
  getPsmMasm,
  setMasmBaseUrl,
  getMasmBaseUrl,
  setEmbeddedMultisigMasm,
  setEmbeddedPsmMasm,
} from './account/index.js';

// =============================================================================
// Types
// =============================================================================

export type {
  // Account types
  MultisigAccountState,
  MultisigAccount as MultisigAccountType, // Deprecated alias
  MultisigConfig,
  CreateAccountResult,

  // Proposal types
  Proposal,
  ProposalStatus,
  ProposalSignatureEntry,
  ProposalMetadata,
  ProposalType,
  ExportedProposal,

  // Transaction types
  TransactionType,

  // Signature types
  FalconSignature,
  Signer,

  // PSM API types
  AuthConfig,
  StorageType,
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
