/**
 * @openzeppelin/psm-client
 *
 * TypeScript HTTP client for Private State Manager (PSM) server.
 *
 * @example
 * ```typescript
 * import { PsmHttpClient, type Signer } from '@openzeppelin/psm-client';
 *
 * const client = new PsmHttpClient('http://localhost:3000');
 *
 * // Get server pubkey (unauthenticated)
 * const pubkey = await client.getPubkey();
 *
 * // Set signer for authenticated requests
 * client.setSigner(mySigner);
 *
 * // Configure an account
 * await client.configure({ account_id, auth, initial_state, storage_type });
 *
 * // Get account state
 * const state = await client.getState(accountId);
 *
 * // Work with delta proposals
 * const proposals = await client.getDeltaProposals(accountId);
 * ```
 */

// HTTP Client
export { PsmHttpClient, PsmHttpError } from './http.js';

// Types
export type {
  // Signer interface
  Signer,

  // Signature types
  FalconSignature,
  CosignerSignature,

  // PSM API types
  AuthConfig,
  StorageType,
  DeltaStatus,
  DeltaObject,
  ExecutionDelta,
  StateObject,
  ProposalMetadata,

  // Request/Response types
  ConfigureRequest,
  ConfigureResponse,
  PubkeyResponse,
  DeltaProposalRequest,
  DeltaProposalResponse,
  ProposalsResponse,
  SignProposalRequest,
} from './types.js';
