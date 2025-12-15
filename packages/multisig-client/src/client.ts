/**
 * High-level client for Miden multisig operations via HTTP.
 *
 * This is the TypeScript equivalent of the Rust `MultisigClient` from
 * `miden-multisig-client`, using HTTP instead of gRPC.
 */

import { PsmHttpClient } from './transport/http.js';
import type {
  AuthConfig,
  DeltaObject,
  DeltaStatus,
  ExportedProposal,
  FalconSignature,
  MultisigAccount,
  Proposal,
  ProposalSignatureEntry,
  ProposalStatus,
  Signer,
  StateObject,
  StorageType,
} from './types.js';

/**
 * Configuration for the MultisigClient.
 */
export interface MultisigClientConfig {
  /** PSM server endpoint (e.g., 'http://localhost:8080') */
  psmEndpoint: string;
  /** The signer for signing requests */
  signer: Signer;
}

/**
 * High-level client for managing multisig accounts and proposals.
 *
 * This client provides the same API as the Rust `MultisigClient` but uses
 * HTTP instead of gRPC for communication with the PSM server.
 */
export class MultisigClient {
  private readonly http: PsmHttpClient;
  private readonly signer: Signer;
  private account: MultisigAccount | null = null;
  private proposals: Map<string, Proposal> = new Map();

  constructor(config: MultisigClientConfig) {
    this.http = new PsmHttpClient(config.psmEndpoint);
    this.signer = config.signer;
    this.http.setSigner(config.signer);
  }

  // ===========================================================================
  // Account Management
  // ===========================================================================

  /**
   * Get the current account, if one is loaded.
   */
  getAccount(): MultisigAccount | null {
    return this.account;
  }

  /**
   * Get the signer's commitment.
   */
  getSignerCommitment(): string {
    return this.signer.commitment;
  }

  /**
   * Get the PSM server's public key commitment.
   */
  async getPsmPubkey(): Promise<string> {
    return this.http.getPubkey();
  }

  /**
   * Register a new multisig account on PSM.
   *
   * @param accountId - The account ID (must be a valid on-chain account)
   * @param threshold - Number of signatures required
   * @param cosignerCommitments - All cosigner public key commitments (including self)
   * @param initialStateBase64 - Base64-encoded serialized account state
   */
  async registerAccount(
    accountId: string,
    threshold: number,
    cosignerCommitments: string[],
    initialStateBase64: string
  ): Promise<MultisigAccount> {
    const auth: AuthConfig = {
      MidenFalconRpo: {
        cosigner_commitments: cosignerCommitments,
      },
    };

    const response = await this.http.configure({
      account_id: accountId,
      auth,
      initial_state: { data: initialStateBase64, account_id: accountId },
      storage_type: 'Filesystem' as StorageType,
    });

    if (!response.success) {
      throw new Error(`Failed to register account: ${response.message}`);
    }

    this.account = {
      id: accountId,
      nonce: 0,
      threshold,
      cosignerCommitments,
    };

    return this.account;
  }

  /**
   * Load an existing account from PSM.
   *
   * @param accountId - The account ID to load
   * @param threshold - The account's threshold (must be known)
   * @param cosignerCommitments - The account's cosigner commitments (must be known)
   */
  async loadAccount(
    accountId: string,
    threshold: number,
    cosignerCommitments: string[]
  ): Promise<MultisigAccount> {
    // Verify account exists on PSM
    await this.http.getState(accountId);

    this.account = {
      id: accountId,
      nonce: 0,
      threshold,
      cosignerCommitments,
    };

    // Sync proposals to get current state
    await this.syncProposals();

    return this.account;
  }

  /**
   * Get account state from PSM.
   */
  async getAccountState(accountId: string): Promise<StateObject> {
    return this.http.getState(accountId);
  }

  // ===========================================================================
  // Proposal Management
  // ===========================================================================

  /**
   * Create a new proposal from a TransactionSummary (base64 encoded).
   *
   * @param nonce - The nonce for this transaction
   * @param txSummaryBase64 - Base64-encoded transaction summary
   * @returns The created proposal
   */
  async createProposal(nonce: number, txSummaryBase64: string): Promise<Proposal> {
    if (!this.account) {
      throw new Error('No account loaded. Call registerAccount() or loadAccount() first.');
    }

    const response = await this.http.pushDeltaProposal({
      account_id: this.account.id,
      nonce,
      delta_payload: {
        tx_summary: { data: txSummaryBase64 },
        signatures: [],
      },
    });

    const proposal = this.deltaToProposal(response.delta, response.commitment);
    this.proposals.set(proposal.id, proposal);

    return proposal;
  }

  /**
   * Sign a proposal.
   *
   * @param proposalId - The proposal commitment/ID
   * @param commitmentToSign - The commitment bytes to sign (usually tx_summary hash)
   * @returns The updated proposal
   */
  async signProposal(proposalId: string, commitmentToSign: string): Promise<Proposal> {
    if (!this.account) {
      throw new Error('No account loaded.');
    }

    const signatureHex = this.signer.signCommitment(commitmentToSign);

    const signature: FalconSignature = {
      Falcon: { signature: signatureHex },
    };

    const delta = await this.http.signDeltaProposal({
      account_id: this.account.id,
      commitment: proposalId,
      signature,
    });

    const proposal = this.deltaToProposal(delta, proposalId);
    this.proposals.set(proposal.id, proposal);

    return proposal;
  }

  /**
   * Execute a proposal that has enough signatures.
   *
   * @param proposalId - The proposal commitment/ID
   */
  async executeProposal(proposalId: string): Promise<void> {
    if (!this.account) {
      throw new Error('No account loaded.');
    }

    const proposal = this.proposals.get(proposalId);
    if (!proposal) {
      throw new Error(`Proposal not found: ${proposalId}`);
    }

    if (proposal.status.type === 'pending') {
      throw new Error('Proposal is not ready for execution. Still pending signatures.');
    }

    // Find the delta object from PSM
    const deltas = await this.http.getDeltaProposals(this.account.id);
    const delta = deltas.find((d) => this.computeProposalId(d) === proposalId);

    if (!delta) {
      throw new Error(`Proposal not found on server: ${proposalId}`);
    }

    await this.http.pushDelta(delta);

    // Update local state
    const updatedProposal = this.proposals.get(proposalId);
    if (updatedProposal) {
      updatedProposal.status = { type: 'finalized' };
    }
  }

  /**
   * List all proposals for the current account.
   */
  async listProposals(): Promise<Proposal[]> {
    await this.syncProposals();
    return Array.from(this.proposals.values());
  }

  /**
   * Sync proposals from PSM server.
   */
  async syncProposals(): Promise<void> {
    if (!this.account) {
      return;
    }

    const deltas = await this.http.getDeltaProposals(this.account.id);

    for (const delta of deltas) {
      const proposalId = this.computeProposalId(delta);
      const proposal = this.deltaToProposal(delta, proposalId);
      this.proposals.set(proposal.id, proposal);
    }
  }

  // ===========================================================================
  // Offline/Export Support
  // ===========================================================================

  /**
   * Export a proposal for offline signing.
   */
  async exportProposal(proposalId: string): Promise<ExportedProposal> {
    if (!this.account) {
      throw new Error('No account loaded.');
    }

    const deltas = await this.http.getDeltaProposals(this.account.id);
    const delta = deltas.find((d) => this.computeProposalId(d) === proposalId);

    if (!delta) {
      throw new Error(`Proposal not found: ${proposalId}`);
    }

    const signatures =
      delta.status.status === 'pending'
        ? delta.status.cosigner_sigs.map((s) => ({
            commitment: s.signer_id,
            signatureHex: s.signature.Falcon.signature,
          }))
        : [];

    return {
      accountId: delta.account_id,
      nonce: delta.nonce,
      commitment: proposalId,
      txSummaryBase64: delta.delta_payload.data,
      signatures,
    };
  }

  // ===========================================================================
  // Private Helpers
  // ===========================================================================

  /**
   * Compute a proposal ID from a delta object.
   * This should hash the delta_payload, but for now we use a placeholder.
   */
  private computeProposalId(delta: DeltaObject): string {
    // In a real implementation, this would compute the RPO hash of the delta_payload
    // For now, we use a combination of account_id and nonce as a simple identifier
    return `${delta.account_id}:${delta.nonce}`;
  }

  /**
   * Convert a PSM DeltaObject to our Proposal type.
   */
  private deltaToProposal(delta: DeltaObject, proposalId: string): Proposal {
    const status = this.deltaStatusToProposalStatus(delta.status);

    const signatures: ProposalSignatureEntry[] =
      delta.status.status === 'pending'
        ? delta.status.cosigner_sigs.map((s) => ({
            signerId: s.signer_id,
            signature: s.signature,
            timestamp: s.timestamp,
          }))
        : [];

    return {
      id: proposalId,
      accountId: delta.account_id,
      nonce: delta.nonce,
      status,
      txSummary: delta.delta_payload.data,
      signatures,
    };
  }

  /**
   * Convert PSM status to our ProposalStatus type.
   */
  private deltaStatusToProposalStatus(status: DeltaStatus): ProposalStatus {
    switch (status.status) {
      case 'pending':
        return {
          type: 'pending',
          signaturesCollected: status.cosigner_sigs.length,
          signaturesRequired: this.account?.threshold ?? 1,
          signers: status.cosigner_sigs.map((s) => s.signer_id),
        };
      case 'candidate':
        return { type: 'ready' };
      case 'canonical':
      case 'discarded':
        return { type: 'finalized' };
    }
  }
}
