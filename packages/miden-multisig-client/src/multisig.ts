/**
 * Multisig class representing a created or loaded multisig account.
 *
 * This class wraps a Miden SDK Account and provides PSM integration
 * for proposal management.
 */

import { PsmHttpClient, type DeltaObject, type DeltaStatus, type FalconSignature, type Signer, type StorageType, type AuthConfig, type StateObject } from '@openzeppelin/psm-client';
import type {
  ConsumableNote,
  ExportedProposal,
  MultisigConfig,
  NoteAsset,
  Proposal,
  ProposalMetadata,
  ProposalSignatureEntry,
  ProposalStatus,
} from './types.js';
import type { WebClient, TransactionRequest } from '@demox-labs/miden-sdk';
import {
  Account,
  AccountId,
  AdviceMap,
  FeltArray,
  Signature,
  TransactionSummary,
  Word,
} from '@demox-labs/miden-sdk';
import {
  executeForSummary,
  buildUpdateSignersTransactionRequest,
  buildUpdateSignersTransactionRequestWithSignatures,
  buildUpdatePsmTransactionRequest,
  buildUpdatePsmTransactionRequestWithSignatures,
  buildConsumeNotesTransactionRequest,
  buildConsumeNotesTransactionRequestWithSignatures,
  buildSignatureAdviceEntry,
  signatureHexToBytes,
  normalizeHexWord,
} from './transaction.js';

/**
 * Result of fetching account state from PSM.
 */
export interface AccountState {
  /** Account ID */
  accountId: string;
  /** Current commitment (state hash) */
  commitment: string;
  /** Raw state data (base64-encoded serialized account) */
  stateDataBase64: string;
  /** When the account was created on PSM */
  createdAt: string;
  /** When the account was last updated on PSM */
  updatedAt: string;
}

/**
 * Convert Uint8Array to base64 string.
 */
function uint8ArrayToBase64(bytes: Uint8Array): string {
  let binary = '';
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary);
}

/**
 * Convert base64 string to Uint8Array.
 */
function base64ToUint8Array(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/**
 * Compute the commitment hex string from a base64-encoded transaction summary.
 * This matches the Rust client's behavior: compute proposal ID client-side from tx_summary.
 */
function computeCommitmentFromTxSummary(txSummaryBase64: string): string {
  const bytes = base64ToUint8Array(txSummaryBase64);
  const summary = TransactionSummary.deserialize(bytes);
  const commitment = summary.toCommitment();
  // Convert Word to hex string (0x prefix + 64 hex chars)
  return commitment.toHex();
}

/**
 * Converts an Account to its hex ID format.
 */
function accountIdToHex(account: Account): string {
  const accountId = account.id();

  // Try using toString() first - in Rust, Display trait calls to_hex()
  const str = accountId.toString();

  // If toString() returns hex format (starts with 0x), use it directly
  if (str.startsWith('0x') || str.startsWith('0X')) {
    return str;
  }

  // Otherwise, construct manually from prefix/suffix
  // Based on Rust: format!("0x{:016x}{:016x}", prefix.as_u64(), suffix.as_int()).truncate(32)
  const prefix = accountId.prefix().asInt();
  const suffix = accountId.suffix().asInt();
  const prefixHex = prefix.toString(16).padStart(16, '0');
  const suffixHex = suffix.toString(16).padStart(16, '0');

  // Truncate to 32 chars: 0x (2) + prefix (16) + suffix first 14 chars (14)
  const hex = `0x${prefixHex}${suffixHex.slice(0, 14)}`;
  return hex;
}

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
  /** The Miden SDK Account */
  readonly account: Account | null;

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
    account: Account | null,
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
    this._accountId = accountId ?? (account ? accountIdToHex(account) : '');
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
  // PSM State Management
  // ===========================================================================

  /**
   * Fetch the current account state from PSM.
   *
   * @returns The account state including commitment and serialized data
   */
  async fetchState(): Promise<AccountState> {
    const state: StateObject = await this.psm.getState(this._accountId);

    return {
      accountId: state.account_id,
      commitment: state.commitment,
      stateDataBase64: state.state_json.data,
      createdAt: state.created_at,
      updatedAt: state.updated_at,
    };
  }

  /**
   * Register this multisig account on the PSM server.
   *
   * The initial state must be the serialized Account bytes (base64-encoded).
   * If not provided, the account's serialize() method is used.
   *
   * @param initialStateBase64 - Optional base64-encoded serialized Account.
   *                             If not provided, uses this.account.serialize().
   */
  async registerOnPsm(initialStateBase64?: string): Promise<void> {
    if (!this.account && !initialStateBase64) {
      throw new Error('Cannot register on PSM: no account available and no initial state provided');
    }

    // Serialize the account to bytes and base64-encode
    let stateData: string;
    if (initialStateBase64) {
      stateData = initialStateBase64;
    } else {
      // Account is guaranteed to exist due to the check above
      const accountBytes: Uint8Array = this.account!.serialize();
      // Convert Uint8Array to base64
      stateData = uint8ArrayToBase64(accountBytes);
    }

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
   * Computes proposal IDs client-side by deserializing the tx_summary and computing its commitment.
   * Reads metadata from PSM if available, or preserves local metadata for proposals that already exist.
   */
  async syncProposals(): Promise<Proposal[]> {
    const deltas = await this.psm.getDeltaProposals(this._accountId);

    for (const delta of deltas) {
      // Compute proposal ID (commitment) client-side from tx_summary
      // This matches the Rust client behavior - we don't rely on server returning new_commitment
      const proposalId = computeCommitmentFromTxSummary(delta.delta_payload.tx_summary.data);
      const existingProposal = this.proposals.get(proposalId);
      const proposal = this.deltaToProposal(delta, proposalId);

      // First try to get metadata from PSM (stored with the proposal)
      if (delta.delta_payload.metadata) {
        // Copy all metadata fields from PSM
        proposal.metadata = {
          proposalType: delta.delta_payload.metadata.proposalType,
          targetThreshold: delta.delta_payload.metadata.targetThreshold,
          targetSignerCommitments: delta.delta_payload.metadata.targetSignerCommitments,
          saltHex: delta.delta_payload.metadata.saltHex,
          description: delta.delta_payload.metadata.description,
          // PSM-specific fields
          newPsmPubkey: delta.delta_payload.metadata.newPsmPubkey,
          newPsmEndpoint: delta.delta_payload.metadata.newPsmEndpoint,
          // Consume notes fields
          noteIds: delta.delta_payload.metadata.noteIds,
        };
      } else if (existingProposal?.metadata) {
        // Fall back to local metadata if PSM doesn't have it (legacy proposals)
        proposal.metadata = existingProposal.metadata;
      }
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
   * @param metadata - Optional metadata for execution (target config, salt, etc.)
   */
  async createProposal(nonce: number, txSummaryBase64: string, metadata?: ProposalMetadata): Promise<Proposal> {
    // Include metadata in the PSM request so other signers can retrieve it
    const response = await this.psm.pushDeltaProposal({
      account_id: this._accountId,
      nonce,
      delta_payload: {
        tx_summary: { data: txSummaryBase64 },
        signatures: [],
        metadata: metadata ? {
          proposalType: metadata.proposalType,
          targetThreshold: metadata.targetThreshold!,
          targetSignerCommitments: metadata.targetSignerCommitments!,
          saltHex: metadata.saltHex!,
          description: metadata.description,
          // PSM-specific fields
          newPsmPubkey: metadata.newPsmPubkey,
          newPsmEndpoint: metadata.newPsmEndpoint,
          // Consume notes fields
          noteIds: metadata.noteIds,
        } : undefined,
      },
    });

    const proposal = this.deltaToProposal(response.delta, response.commitment);
    // Attach metadata if provided
    if (metadata) {
      proposal.metadata = metadata;
    }
    this.proposals.set(proposal.id, proposal);

    return proposal;
  }

  /**
   * Create an "add signer" proposal by executing the update_signers script to summary.
   *
   * @param webClient - Initialized Miden WebClient
   * @param newCommitment - Commitment of the new signer (hex)
   * @param nonce - Optional proposal nonce (defaults to Date.now())
   * @param newThreshold - Optional new threshold (defaults to current threshold)
   */
  async createAddSignerProposal(
    webClient: WebClient,
    newCommitment: string,
    nonce?: number,
    newThreshold?: number,
  ): Promise<Proposal> {
    const targetThreshold = newThreshold ?? this.threshold;
    const targetSignerCommitments = [...this.signerCommitments, newCommitment];

    const { request, salt } = await buildUpdateSignersTransactionRequest(
      webClient,
      targetThreshold,
      targetSignerCommitments,
    );

    const summary = await executeForSummary(webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'add_signer',
      targetThreshold,
      targetSignerCommitments,
      saltHex: salt.toHex(),
      description: `Add signer ${newCommitment.slice(0, 10)}...`,
    };

    return this.createProposal(proposalNonce, summaryBase64, metadata);
  }

  /**
   * Create a "remove signer" proposal by executing the update_signers script to summary.
   *
   * @param webClient - Initialized Miden WebClient
   * @param signerToRemove - Commitment of the signer to remove (hex)
   * @param nonce - Optional proposal nonce (defaults to Date.now())
   * @param newThreshold - Optional new threshold (defaults to min of current threshold and new signer count)
   */
  async createRemoveSignerProposal(
    webClient: WebClient,
    signerToRemove: string,
    nonce?: number,
    newThreshold?: number,
  ): Promise<Proposal> {
    // Validate signer exists
    const normalizedRemove = signerToRemove.toLowerCase();
    const signerExists = this.signerCommitments.some(
      (c) => c.toLowerCase() === normalizedRemove
    );
    if (!signerExists) {
      throw new Error(`Signer ${signerToRemove} is not in the current signer list`);
    }

    // Filter out the signer
    const targetSignerCommitments = this.signerCommitments.filter(
      (c) => c.toLowerCase() !== normalizedRemove
    );

    if (targetSignerCommitments.length === 0) {
      throw new Error('Cannot remove the last signer');
    }

    // Auto-adjust threshold if needed
    const targetThreshold = newThreshold ?? Math.min(this.threshold, targetSignerCommitments.length);

    // Validate threshold
    if (targetThreshold < 1 || targetThreshold > targetSignerCommitments.length) {
      throw new Error(
        `Invalid threshold ${targetThreshold}. Must be between 1 and ${targetSignerCommitments.length}`
      );
    }

    const { request, salt } = await buildUpdateSignersTransactionRequest(
      webClient,
      targetThreshold,
      targetSignerCommitments,
    );

    const summary = await executeForSummary(webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'remove_signer',
      targetThreshold,
      targetSignerCommitments,
      saltHex: salt.toHex(),
      description: `Remove signer ${signerToRemove.slice(0, 10)}...`,
    };

    return this.createProposal(proposalNonce, summaryBase64, metadata);
  }

  /**
   * Create a "change threshold" proposal without modifying signers.
   *
   * @param webClient - Initialized Miden WebClient
   * @param newThreshold - The new threshold value
   * @param nonce - Optional proposal nonce (defaults to Date.now())
   */
  async createChangeThresholdProposal(
    webClient: WebClient,
    newThreshold: number,
    nonce?: number,
  ): Promise<Proposal> {
    // Validate threshold
    if (newThreshold < 1 || newThreshold > this.signerCommitments.length) {
      throw new Error(
        `Invalid threshold ${newThreshold}. Must be between 1 and ${this.signerCommitments.length}`
      );
    }

    if (newThreshold === this.threshold) {
      throw new Error('New threshold is the same as current threshold');
    }

    const { request, salt } = await buildUpdateSignersTransactionRequest(
      webClient,
      newThreshold,
      this.signerCommitments,
    );

    const summary = await executeForSummary(webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'change_threshold',
      targetThreshold: newThreshold,
      targetSignerCommitments: this.signerCommitments,
      saltHex: salt.toHex(),
      description: `Change threshold from ${this.threshold} to ${newThreshold}`,
    };

    return this.createProposal(proposalNonce, summaryBase64, metadata);
  }

  /**
   * Create a "switch PSM" proposal to change the PSM provider.
   *
   * @param webClient - Initialized Miden WebClient
   * @param newPsmEndpoint - The new PSM server endpoint URL
   * @param newPsmPubkey - The new PSM server's public key commitment (hex)
   * @param nonce - Optional proposal nonce (defaults to Date.now())
   */
  async createSwitchPsmProposal(
    webClient: WebClient,
    newPsmEndpoint: string,
    newPsmPubkey: string,
    nonce?: number,
  ): Promise<Proposal> {
    const { request, salt } = await buildUpdatePsmTransactionRequest(
      webClient,
      newPsmPubkey,
    );

    const summary = await executeForSummary(webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'switch_psm',
      // Keep current signer config (no change)
      targetThreshold: this.threshold,
      targetSignerCommitments: this.signerCommitments,
      saltHex: salt.toHex(),
      // PSM-specific metadata
      newPsmPubkey,
      newPsmEndpoint,
      description: `Switch PSM to ${newPsmEndpoint}`,
    };

    return this.createProposal(proposalNonce, summaryBase64, metadata);
  }

  /**
   * Create a "consume notes" proposal to consume notes sent to the multisig account.
   *
   * @param webClient - Initialized Miden WebClient
   * @param noteIds - IDs of the notes to consume (hex strings)
   * @param nonce - Optional proposal nonce (defaults to Date.now())
   */
  async createConsumeNotesProposal(
    webClient: WebClient,
    noteIds: string[],
    nonce?: number,
  ): Promise<Proposal> {
    if (noteIds.length === 0) {
      throw new Error('At least one note ID is required');
    }

    const { request, salt } = buildConsumeNotesTransactionRequest(noteIds);

    const summary = await executeForSummary(webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'consume_notes' as const,
      noteIds,
      saltHex: salt.toHex(),
      description: `Consume ${noteIds.length} note(s)`,
    };

    return this.createProposal(proposalNonce, summaryBase64, metadata);
  }

  /**
   * Get notes that can be consumed by this multisig account.
   *
   * Returns a list of notes that are committed on-chain and can be consumed
   * immediately by the multisig account.
   *
   * @param webClient - Initialized Miden WebClient
   */
  async getConsumableNotes(
    webClient: WebClient,
  ): Promise<ConsumableNote[]> {
    const accountId = AccountId.fromHex(this._accountId);

    // Get consumable notes for this account
    const consumableRecords = await webClient.getConsumableNotes(accountId);

    // Convert to our simplified ConsumableNote type
    const notes: ConsumableNote[] = [];
    for (const record of consumableRecords) {
      const inputNote = record.inputNoteRecord();
      const consumability = record.noteConsumability();

      // Only include notes that can be consumed now (consumableAfterBlock is undefined/null)
      const canConsumeNow = consumability.some(
        (c) => c.accountId().toString().toLowerCase() === this._accountId.toLowerCase() &&
               c.consumableAfterBlock() === undefined
      );

      if (canConsumeNow) {
        const noteId = inputNote.id().toString();
        const details = inputNote.details();
        const fungibleAssets = details.assets().fungibleAssets();

        // Extract assets
        const assets: NoteAsset[] = [];
        for (const asset of fungibleAssets) {
          assets.push({
            faucetId: asset.faucetId().toString(),
            amount: asset.amount(),
          });
        }

        notes.push({ id: noteId, assets });
      }
    }

    return notes;
  }

  /**
   * Sign a proposal.
   *
   * The proposalId is the tx_summary commitment hex, which is what gets signed.
   * This matches the Rust client behavior where proposal.id == tx_summary.to_commitment().
   *
   * @param proposalId - The proposal commitment/ID (this is also what gets signed)
   */
  async signProposal(proposalId: string): Promise<Proposal> {
    // Get existing proposal to preserve metadata
    const existingProposal = this.proposals.get(proposalId);

    // The proposal ID is the tx_summary commitment - this is what we sign
    const signatureHex = this.signer.signCommitment(proposalId);

    const signature: FalconSignature = {
      scheme: 'falcon',
      signature: signatureHex,
    };

    const delta = await this.psm.signDeltaProposal({
      account_id: this._accountId,
      commitment: proposalId,
      signature,
    });

    const proposal = this.deltaToProposal(delta, proposalId);

    // Preserve metadata from existing proposal (e.g., target config for signer updates)
    if (existingProposal?.metadata) {
      proposal.metadata = existingProposal.metadata;
    }

    this.proposals.set(proposal.id, proposal);

    return proposal;
  }

  /**
   * Execute a proposal that has enough signatures.
   *
   * This performs the full on-chain execution flow:
   * 1. Push delta to PSM to get acknowledgment signature (except for switch_psm)
   * 2. Build advice map with all cosigner signatures + PSM ack
   * 3. Execute, prove, submit, and apply the transaction
   *
   * @param proposalId - The proposal commitment/ID
   * @param webClient - Initialized Miden WebClient for transaction execution
   */
  async executeProposal(
    proposalId: string,
    webClient: WebClient,
  ): Promise<void> {
    const proposal = this.proposals.get(proposalId);
    if (!proposal) {
      throw new Error(`Proposal not found: ${proposalId}`);
    }

    if (proposal.status.type === 'pending') {
      throw new Error('Proposal is not ready for execution. Still pending signatures.');
    }

    const deltas = await this.psm.getDeltaProposals(this._accountId);
    const delta = deltas.find(
      (d) => computeCommitmentFromTxSummary(d.delta_payload.tx_summary.data) === proposalId
    );

    if (!delta) {
      throw new Error(`Proposal not found on server: ${proposalId}`);
    }

    const executionDelta = {
      ...delta,
      delta_payload: delta.delta_payload.tx_summary,
    };

    const pushResult = await this.psm.pushDelta(executionDelta);
    const ackSigHex = pushResult.ack_sig;
    if (!ackSigHex) {
      throw new Error('PSM did not return acknowledgment signature');
    }

    // Deserialize the tx_summary to get the salt and commitment
    const txSummaryBytes = base64ToUint8Array(delta.delta_payload.tx_summary.data);
    const txSummary = TransactionSummary.deserialize(txSummaryBytes);
    const salt = txSummary.salt();
    // Store commitment as hex to recreate Word for each signature (WASM objects get consumed)
    const txCommitmentHex = txSummary.toCommitment().toHex();

    // Build advice map with all signatures
    const adviceMap = new AdviceMap();

    if (delta.status.status === 'pending') {
      for (const cosignerSig of delta.status.cosigner_sigs) {
        const signerCommitment = Word.fromHex(normalizeHexWord(cosignerSig.signer_id));
        const sigBytes = signatureHexToBytes(cosignerSig.signature.signature);
        const signature = Signature.deserialize(sigBytes);
        const txCommitment = Word.fromHex(normalizeHexWord(txCommitmentHex));
        const { key, values } = buildSignatureAdviceEntry(
          signerCommitment,
          txCommitment,
          signature
        );
        adviceMap.insert(key, new FeltArray(values));
      }
    }

    // Add PSM ack signature
    const psmCommitment = Word.fromHex(normalizeHexWord(this.psmCommitment));
    const ackSigBytes = signatureHexToBytes(ackSigHex);
    const ackSignature = Signature.deserialize(ackSigBytes);
    const txCommitmentForAck = Word.fromHex(normalizeHexWord(txCommitmentHex));
    const { key: ackKey, values: ackValues } = buildSignatureAdviceEntry(
      psmCommitment,
      txCommitmentForAck,
      ackSignature
    );
    adviceMap.insert(ackKey, new FeltArray(ackValues));

    // Build the final transaction request based on proposal type
    let finalRequest: TransactionRequest;

    const proposalType = proposal.metadata?.proposalType as string | undefined;

    if (proposalType === 'consume_notes') {
      // Consume notes proposal
      if (!proposal.metadata?.noteIds || proposal.metadata.noteIds.length === 0) {
        throw new Error('Proposal missing noteIds. Was it created with createConsumeNotesProposal?');
      }
      finalRequest = buildConsumeNotesTransactionRequestWithSignatures(
        proposal.metadata.noteIds,
        salt,
        adviceMap,
      );
    } else if (proposalType === 'switch_psm') {
      // Switch PSM proposal
      if (!proposal.metadata?.newPsmPubkey) {
        throw new Error('Proposal missing newPsmPubkey. Was it created with createSwitchPsmProposal?');
      }
      finalRequest = await buildUpdatePsmTransactionRequestWithSignatures(
        webClient,
        proposal.metadata.newPsmPubkey,
        salt,
        adviceMap,
      );
    } else {
      // Signer update proposals (add_signer, remove_signer, change_threshold)
      if (!proposal.metadata?.targetThreshold || !proposal.metadata?.targetSignerCommitments) {
        throw new Error('Proposal missing metadata (targetThreshold/targetSignerCommitments). Was it created with createAddSignerProposal?');
      }
      finalRequest = await buildUpdateSignersTransactionRequestWithSignatures(
        webClient,
        proposal.metadata.targetThreshold,
        proposal.metadata.targetSignerCommitments,
        salt,
        adviceMap,
      );
    }

    // Execute, prove, submit, apply
    const accountId = AccountId.fromHex(this._accountId);
    const result = await webClient.executeTransaction(accountId, finalRequest);
    const proven = await webClient.proveTransaction(result, null);
    const submissionHeight = await webClient.submitProvenTransaction(proven, result);
    await webClient.applyTransaction(result, submissionHeight);
    await webClient.syncState();

    proposal.status = { type: 'finalized' };
  }

  /**
   * Export a proposal for offline signing (fetches from PSM).
   * @deprecated Use exportProposalToJson for offline/side-channel sharing
   */
  async exportProposal(proposalId: string): Promise<ExportedProposal> {
    const deltas = await this.psm.getDeltaProposals(this._accountId);
    // Find delta by computing commitment from tx_summary (client-side)
    const delta = deltas.find((d) => computeCommitmentFromTxSummary(d.delta_payload.tx_summary.data) === proposalId);

    if (!delta) {
      throw new Error(`Proposal not found: ${proposalId}`);
    }

    const signatures =
      delta.status.status === 'pending'
        ? delta.status.cosigner_sigs.map((s) => ({
            commitment: s.signer_id,
            signatureHex: s.signature.signature,
          }))
        : [];

    return {
      accountId: delta.account_id,
      nonce: delta.nonce,
      commitment: proposalId,
      txSummaryBase64: delta.delta_payload.tx_summary.data,
      signatures,
    };
  }

  /**
   * Export a proposal to JSON for side-channel sharing.
   * This exports from local cache and includes metadata, so PSM is not required.
   * Useful when switching PSM providers and coordination via PSM is unavailable.
   *
   * @param proposalId - The proposal commitment/ID
   * @returns JSON string that can be shared and imported by other signers
   */
  exportProposalToJson(proposalId: string): string {
    const proposal = this.proposals.get(proposalId);
    if (!proposal) {
      throw new Error(`Proposal not found in local cache: ${proposalId}`);
    }

    const exported: ExportedProposal = {
      accountId: proposal.accountId,
      nonce: proposal.nonce,
      commitment: proposal.id,
      txSummaryBase64: proposal.txSummary,
      signatures: proposal.signatures.map((s) => ({
        commitment: s.signerId,
        signatureHex: s.signature.signature,
        timestamp: s.timestamp,
      })),
      metadata: proposal.metadata,
    };

    return JSON.stringify(exported, null, 2);
  }

  /**
   * Import a proposal from JSON (exported via exportProposalToJson).
   * This adds the proposal to local cache for signing/execution.
   * Useful when receiving proposals via side-channel when PSM is unavailable.
   *
   * @param json - JSON string from exportProposalToJson
   * @returns The imported proposal
   */
  importProposal(json: string): Proposal {
    const exported: ExportedProposal = JSON.parse(json);

    // Validate the imported proposal
    if (!exported.accountId || !exported.txSummaryBase64 || !exported.commitment) {
      throw new Error('Invalid proposal JSON: missing required fields');
    }

    if (exported.accountId.toLowerCase() !== this._accountId.toLowerCase()) {
      throw new Error(`Proposal is for a different account: ${exported.accountId}`);
    }

    // Verify the commitment matches the tx_summary
    const computedCommitment = computeCommitmentFromTxSummary(exported.txSummaryBase64);
    if (computedCommitment !== exported.commitment) {
      throw new Error('Invalid proposal: commitment does not match tx_summary');
    }

    // Convert to Proposal
    const signaturesCollected = exported.signatures.length;
    const signaturesRequired = this.threshold;
    const status: ProposalStatus = signaturesCollected >= signaturesRequired
      ? { type: 'ready' }
      : {
          type: 'pending',
          signaturesCollected,
          signaturesRequired,
          signers: exported.signatures.map((s) => s.commitment),
        };

    const proposal: Proposal = {
      id: exported.commitment,
      accountId: exported.accountId,
      nonce: exported.nonce,
      status,
      txSummary: exported.txSummaryBase64,
      signatures: exported.signatures.map((s) => ({
        signerId: s.commitment,
        signature: { scheme: 'falcon' as const, signature: s.signatureHex },
        timestamp: s.timestamp || new Date().toISOString(),
      })),
      metadata: exported.metadata,
    };

    // Add to local cache
    this.proposals.set(proposal.id, proposal);

    return proposal;
  }

  /**
   * Sign an imported proposal and return updated JSON for sharing.
   * Use this when PSM is unavailable and coordination happens via side-channel.
   *
   * @param proposalId - The proposal commitment/ID
   * @returns Updated JSON string with the new signature included
   */
  signProposalOffline(proposalId: string): string {
    const proposal = this.proposals.get(proposalId);
    if (!proposal) {
      throw new Error(`Proposal not found: ${proposalId}`);
    }

    // Check if already signed
    const alreadySigned = proposal.signatures.some(
      (s) => s.signerId.toLowerCase() === this.signer.commitment.toLowerCase()
    );
    if (alreadySigned) {
      throw new Error('You have already signed this proposal');
    }

    // Sign the commitment
    const signatureHex = this.signer.signCommitment(proposalId);

    // Add signature to local proposal
    proposal.signatures.push({
      signerId: this.signer.commitment,
      signature: { scheme: 'falcon', signature: signatureHex },
      timestamp: new Date().toISOString(),
    });

    // Update status
    const signaturesCollected = proposal.signatures.length;
    if (signaturesCollected >= this.threshold) {
      proposal.status = { type: 'ready' };
    } else if (proposal.status.type === 'pending') {
      proposal.status = {
        type: 'pending',
        signaturesCollected,
        signaturesRequired: this.threshold,
        signers: proposal.signatures.map((s) => s.signerId),
      };
    }

    // Return updated JSON
    return this.exportProposalToJson(proposalId);
  }

  // ===========================================================================
  // Private Helpers
  // ===========================================================================

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
      txSummary: delta.delta_payload.tx_summary.data,
      signatures,
    };
  }

  private deltaStatusToProposalStatus(status: DeltaStatus): ProposalStatus {
    switch (status.status) {
      case 'pending': {
        const signaturesCollected = status.cosigner_sigs.length;
        const signaturesRequired = this.threshold;
        // If we have enough signatures, the proposal is ready for execution
        if (signaturesCollected >= signaturesRequired) {
          return { type: 'ready' };
        }
        return {
          type: 'pending',
          signaturesCollected,
          signaturesRequired,
          signers: status.cosigner_sigs.map((s) => s.signer_id),
        };
      }
      case 'candidate':
        return { type: 'ready' };
      case 'canonical':
      case 'discarded':
        return { type: 'finalized' };
    }
  }
}
