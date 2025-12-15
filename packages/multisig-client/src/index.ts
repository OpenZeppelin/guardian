/**
 * @openzeppelin/multisig-client
 *
 * TypeScript SDK for Miden multisig accounts with PSM (Private State Manager) integration.
 * This provides the same functionality as the Rust `miden-multisig-client` crate
 * but for browser and Node.js environments.
 *
 * @example
 * ```typescript
 * import {
 *   createMultisigAccount,
 *   generateKey,
 *   createSigner,
 *   PsmHttpClient,
 * } from '@openzeppelin/multisig-client';
 * import { WebClient } from '@demox-labs/miden-sdk';
 *
 * // Initialize WebClient
 * const webClient = await WebClient.createClient('https://rpc.testnet.miden.io:443');
 * await webClient.syncState();
 *
 * // Generate a key
 * const keyEntry = generateKey('My Key');
 *
 * // Create a signer
 * const signer = createSigner(keyEntry);
 *
 * // Create a multisig account
 * const config = { threshold: 2, signerCommitments: [...], psmCommitment: '...' };
 * const { account } = await createMultisigAccount(webClient, config);
 *
 * // Use PSM client
 * const psmClient = new PsmHttpClient('http://localhost:3000');
 * psmClient.setSigner(signer);
 * ```
 */

// =============================================================================
// Utilities
// =============================================================================

export { clearIndexedDB } from './miden.js';

// =============================================================================
// Client Classes
// =============================================================================

export { MultisigClient, type MultisigClientConfig } from './client.js';
export { MultisigClientBuilder } from './builder.js';
export { MultisigAccount } from './account.js';

// =============================================================================
// Transport Layer
// =============================================================================

export { PsmHttpClient, PsmHttpError } from './transport/index.js';

// =============================================================================
// Key Management
// =============================================================================

export {
  generateKey,
  loadKeys,
  loadSecretKey,
  deleteKey,
  getKey,
  renameKey,
  clearKeystore,
  type KeyEntry,
} from './keystore.js';

// =============================================================================
// Signer
// =============================================================================

export {
  createSigner,
  createSignerFromSecretKey,
} from './signer.js';

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
