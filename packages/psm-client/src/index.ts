/**
 * @openzeppelin/psm-client
 *
 * TypeScript client for interacting with the Miden Private State Manager (PSM)
 * server via HTTP. This provides the same functionality as the Rust
 * `miden-multisig-client` crate but for browser and Node.js environments.
 */

export { MultisigClient, type MultisigClientConfig } from './client.js';
export { PsmHttpClient, PsmHttpError } from './http.js';
export type {
  // Account types
  MultisigAccount,

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
