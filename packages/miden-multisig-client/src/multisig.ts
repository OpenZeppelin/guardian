/**
 * Multisig class representing a created or loaded multisig account.
 *
 * This class wraps a Miden SDK Account and provides PSM integration
 * for proposal management.
 */

import { PsmHttpClient, type DeltaObject, type DeltaStatus, type FalconSignature, type Signer, type StorageType, type AuthConfig } from '@openzeppelin/psm-client';
import type {
  ExportedProposal,
  MultisigConfig,
  Proposal,
  ProposalSignatureEntry,
  ProposalStatus,
} from './types.js';

/**
 * Represents a multisig account with PSM integration.
 *
 * @example
 * ```typescript
 * // Create via MultisigClient
 * const multisig = await client.create(config, signer);
 *
 * // Register on PSM
 * await multisig.registerOnPsm();
 *
 * // Work with proposals
 * const proposals = await multisig.syncProposals();
 * ```
 */
export class Multisig {
  /** The Miden SDK Account (null if loaded from PSM) */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  readonly account: any | null;

  /** Number of signatures required */
  readonly threshold: number;

  /** All signer commitments */
  readonly signerCommitments: string[];

  /** PSM server public key commitment */
  readonly psmCommitment: string;

  private readonly psm: PsmHttpClient;
  private readonly signer: Signer;
  private readonly _accountId: string;
  private proposals: Map<string, Proposal> = new Map();

  constructor(
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    account: any | null,
    config: MultisigConfig,
    psm: PsmHttpClient,
    signer: Signer,
    accountId?: string
  ) {
    this.account = account;
    this.threshold = config.threshold;
    this.signerCommitments = config.signerCommitments;
    this.psmCommitment = config.psmCommitment;
    this.psm = psm;
    this.signer = signer;
    this._accountId = accountId ?? account?.id().toString() ?? '';
  }

  /** The account ID as a string */
  get accountId(): string {
    return this._accountId;
  }

  /** The signer's commitment */
  get signerCommitment(): string {
    return this.signer.commitment;
  }

  // ===========================================================================
  // PSM Registration
  // ===========================================================================

  /**
   * Register this multisig account on the PSM server.
   *
   * @param initialStateBase64 - Optional base64-encoded initial state.
   *                             If not provided, a default state is created.
   */
  async registerOnPsm(initialStateBase64?: string): Promise<void> {
    const stateData =
      initialStateBase64 ??
      btoa(
        JSON.stringify({
          account_id: this._accountId,
          nonce: this.account?.nonce().toString() ?? '0',
        })
      );

    const auth: AuthConfig = {
      MidenFalconRpo: {
        cosigner_commitments: this.signerCommitments,
      },
    };

    const response = await this.psm.configure({
      account_id: this._accountId,
      auth,
      initial_state: { data: stateData, account_id: this._accountId },
      storage_type: 'Filesystem' as StorageType,
    });

    if (!response.success) {
      throw new Error(`Failed to register on PSM: ${response.message}`);
    }
  }

  // ===========================================================================
  // Proposal Management
  // ===========================================================================

  /**
   * Sync proposals from the PSM server.
   */
  async syncProposals(): Promise<Proposal[]> {
    const deltas = await this.psm.getDeltaProposals(this._accountId);

    for (const delta of deltas) {
      const proposalId = this.computeProposalId(delta);
      const proposal = this.deltaToProposal(delta, proposalId);
      this.proposals.set(proposal.id, proposal);
    }

    return Array.from(this.proposals.values());
  }

  /**
   * List all known proposals (call syncProposals first for latest).
   */
  listProposals(): Proposal[] {
    return Array.from(this.proposals.values());
  }

  /**
   * Create a new proposal.
   *
   * @param nonce - The nonce for this transaction
   * @param txSummaryBase64 - Base64-encoded transaction summary
   */
  async createProposal(nonce: number, txSummaryBase64: string): Promise<Proposal> {
    const response = await this.psm.pushDeltaProposal({
      account_id: this._accountId,
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
   * @param commitmentToSign - The commitment bytes to sign
   */
  async signProposal(proposalId: string, commitmentToSign: string): Promise<Proposal> {
    const signatureHex = this.signer.signCommitment(commitmentToSign);

    const signature: FalconSignature = {
      Falcon: { signature: signatureHex },
    };

    const delta = await this.psm.signDeltaProposal({
      account_id: this._accountId,
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
    const proposal = this.proposals.get(proposalId);
    if (!proposal) {
      throw new Error(`Proposal not found: ${proposalId}`);
    }

    if (proposal.status.type === 'pending') {
      throw new Error('Proposal is not ready for execution. Still pending signatures.');
    }

    const deltas = await this.psm.getDeltaProposals(this._accountId);
    const delta = deltas.find((d) => this.computeProposalId(d) === proposalId);

    if (!delta) {
      throw new Error(`Proposal not found on server: ${proposalId}`);
    }

    await this.psm.pushDelta(delta);

    const updatedProposal = this.proposals.get(proposalId);
    if (updatedProposal) {
      updatedProposal.status = { type: 'finalized' };
    }
  }

  /**
   * Export a proposal for offline signing.
   */
  async exportProposal(proposalId: string): Promise<ExportedProposal> {
    const deltas = await this.psm.getDeltaProposals(this._accountId);
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

  private computeProposalId(delta: DeltaObject): string {
    return `${delta.account_id}:${delta.nonce}`;
  }

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

  private deltaStatusToProposalStatus(status: DeltaStatus): ProposalStatus {
    switch (status.status) {
      case 'pending':
        return {
          type: 'pending',
          signaturesCollected: status.cosigner_sigs.length,
          signaturesRequired: this.threshold,
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
