/**
 * Multisig class representing a created or loaded multisig account.
 *
 * This class wraps a Miden SDK Account and provides PSM integration
 * for proposal management.
 */

import { PsmHttpClient, type DeltaObject, type DeltaStatus, type FalconSignature, type Signer, type AuthConfig, type StateObject, type ProposalMetadata as PsmProposalMetadata } from '@openzeppelin/psm-client';
import type {
  ConsumableNote,
  ExportedProposal,
  MultisigConfig,
  NoteAsset,
  Proposal,
  ProposalMetadata,
  ProposalSignatureEntry,
  ProposalStatus,
  ProposalType,
} from './types.js';
import type { ProcedureName } from './procedures.js';
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
  buildUpdatePsmTransactionRequest,
  buildConsumeNotesTransactionRequest,
  buildP2idTransactionRequest,
} from './transaction.js';
import {
  base64ToUint8Array,
  uint8ArrayToBase64,
  normalizeHexWord,
} from './utils/encoding.js';
import { buildSignatureAdviceEntry, signatureHexToBytes } from './utils/signature.js';
import { computeCommitmentFromTxSummary, accountIdToHex } from './multisig/helpers.js';

/**
 * Result of fetching account state from PSM.
 */
export interface AccountState {
  /** Account ID */
  accountId: string;
  /** Current commitment */
  commitment: string;
  /** Raw state data (base64-encoded serialized account) */
  stateDataBase64: string;
  createdAt: string;
  updatedAt: string;
}

/**
 * Represents a multisig account with PSM integration.
 */
export class Multisig {
  readonly account: Account | null;
  readonly threshold: number;
  readonly signerCommitments: string[];
  readonly psmCommitment: string;
  readonly procedureThresholds: Map<ProcedureName, number>;

  private psm: PsmHttpClient;
  private readonly signer: Signer;
  private readonly webClient: WebClient;
  private readonly _accountId: string;
  private proposals: Map<string, Proposal> = new Map();

  constructor(
    account: Account | null,
    config: MultisigConfig,
    psm: PsmHttpClient,
    signer: Signer,
    webClient: WebClient,
    accountId?: string
  ) {
    this.account = account;
    this.threshold = config.threshold;
    this.signerCommitments = config.signerCommitments;
    this.psmCommitment = config.psmCommitment;
    this.procedureThresholds = new Map(
      (config.procedureThresholds ?? []).map((pt) => [pt.procedure, pt.threshold])
    );
    this.psm = psm;
    this.signer = signer;
    this.webClient = webClient;
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

  /**
   * Maps a proposal type to the procedure that determines its threshold.
   */
  private getProposalProcedure(proposalType: ProposalType): ProcedureName | null {
    switch (proposalType) {
      case 'p2id':
        return 'send_asset';
      case 'consume_notes':
        return 'receive_asset';
      case 'add_signer':
      case 'remove_signer':
      case 'change_threshold':
        return 'update_signers';
      case 'switch_psm':
        return 'update_psm';
      default:
        return null;
    }
  }

  /**
   * Get the effective threshold for a given proposal type.
   * Returns the procedure-specific threshold if configured, otherwise the default threshold.
   *
   * @param proposalType - The type of proposal
   * @returns The threshold that applies to this proposal type
   */
  getEffectiveThreshold(proposalType: ProposalType): number {
    if (this.procedureThresholds.size === 0) {
      return this.threshold;
    }

    const procedure = this.getProposalProcedure(proposalType);
    if (!procedure) {
      return this.threshold;
    }

    return this.procedureThresholds.get(procedure) ?? this.threshold;
  }

  /**
   * Update the PSM client used by this Multisig instance.
   *
   * @param psmClient - The new PSM HTTP client
   */
  setPsmClient(psmClient: PsmHttpClient): void {
    this.psm = psmClient;
    this.psm.setSigner(this.signer);
  }

  /**
   * Fetch the current account state from PSM.
   *
   * @returns The account state including commitment and serialized data
   */
  async fetchState(): Promise<AccountState> {
    const state: StateObject = await this.psm.getState(this._accountId);

    return {
      accountId: state.accountId,
      commitment: state.commitment,
      stateDataBase64: state.stateJson.data,
      createdAt: state.createdAt,
      updatedAt: state.updatedAt,
    };
  }

  /**
   * Sync account state from PSM into the local WebClient store.
   *
   * If the PSM commitment differs from the local commitment (or the account
   * is missing locally), the local store is overwritten with the PSM state.
   */
  async syncState(): Promise<AccountState> {
    const state = await this.fetchState();
    const accountId = AccountId.fromHex(this._accountId);
    const localAccount = await this.webClient.getAccount(accountId);

    const psmCommitment = normalizeHexWord(state.commitment);
    const localCommitment = localAccount
      ? normalizeHexWord(localAccount.commitment().toHex())
      : null;

    if (!localAccount || localCommitment !== psmCommitment) {
      const accountBytes = base64ToUint8Array(state.stateDataBase64);
      const account = Account.deserialize(accountBytes);
      await this.webClient.newAccount(account, true);
    }

    return state;
  }

  /**
   * Register this multisig account on the PSM server.
   *
   * The initial state must be the serialized Account bytes (base64-encoded).
   * If not provided, the account's serialize() method is used.
   *
   * @param initialStateBase64 - Optional base64-encoded serialized Account.¡
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
      const accountBytes: Uint8Array = this.account!.serialize();
      stateData = uint8ArrayToBase64(accountBytes);
    }

    const auth: AuthConfig = {
      MidenFalconRpo: {
        cosigner_commitments: this.signerCommitments,
      },
    };

    const response = await this.psm.configure({
      accountId: this._accountId,
      auth,
      initialState: { data: stateData, accountId: this._accountId },
    });

    if (!response.success) {
      throw new Error(`Failed to register on PSM: ${response.message}`);
    }
  }

  /**
   * Sync proposals from the PSM server.
   */
  async syncProposals(): Promise<Proposal[]> {
    const deltas = await this.psm.getDeltaProposals(this._accountId);

    for (const delta of deltas) {
      const proposalId = computeCommitmentFromTxSummary(delta.deltaPayload.txSummary.data);
      const existingProposal = this.proposals.get(proposalId);

      const resolvedMetadata =
        existingProposal?.metadata ??
        (delta.deltaPayload.metadata
          ? this.fromPsmMetadata(delta.deltaPayload.metadata)
          : undefined);
      if (!resolvedMetadata) {
        throw new Error('Missing proposal metadata from PSM');
      }

      const proposal = this.deltaToProposal(delta, proposalId, resolvedMetadata, existingProposal?.signatures);

      this.proposals.set(proposal.id, proposal);
    }

    return Array.from(this.proposals.values());
  }

  /**
   * List all known proposals
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
  async createProposal(nonce: number, txSummaryBase64: string, metadata: ProposalMetadata): Promise<Proposal> {
    const psmMetadata = this.buildPsmMetadata(metadata);

    const response = await this.psm.pushDeltaProposal({
      accountId: this._accountId,
      nonce,
      deltaPayload: {
        txSummary: { data: txSummaryBase64 },
        signatures: [],
        metadata: psmMetadata,
      },
    });

    const proposal = this.deltaToProposal(response.delta, response.commitment, metadata);
    this.proposals.set(proposal.id, proposal);

    return proposal;
  }

  /**
   * Create an "add signer" proposal.
   *
   * @param newCommitment - Commitment of the new signer (hex)
   * @param nonce - Optional proposal nonce (defaults to Date.now())
   * @param newThreshold - Optional new threshold (defaults to current threshold)
   */
  async createAddSignerProposal(
    newCommitment: string,
    nonce?: number,
    newThreshold?: number,
  ): Promise<Proposal> {
    const targetThreshold = newThreshold ?? this.threshold;
    const targetSignerCommitments = [...this.signerCommitments, newCommitment];

    const { request, salt } = await buildUpdateSignersTransactionRequest(
      this.webClient,
      targetThreshold,
      targetSignerCommitments,
    );

    const summary = await executeForSummary(this.webClient, this._accountId, request);
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
   * @param signerToRemove - Commitment of the signer to remove (hex)
   * @param nonce - Optional proposal nonce (defaults to Date.now())
   * @param newThreshold - Optional new threshold (defaults to min of current threshold and new signer count)
   */
  async createRemoveSignerProposal(
    signerToRemove: string,
    nonce?: number,
    newThreshold?: number,
  ): Promise<Proposal> {
    const normalizedRemove = signerToRemove.toLowerCase();
    const signerExists = this.signerCommitments.some(
      (c) => c.toLowerCase() === normalizedRemove
    );
    if (!signerExists) {
      throw new Error(`Signer ${signerToRemove} is not in the current signer list`);
    }

    const targetSignerCommitments = this.signerCommitments.filter(
      (c) => c.toLowerCase() !== normalizedRemove
    );

    if (targetSignerCommitments.length === 0) {
      throw new Error('Cannot remove the last signer');
    }

    const targetThreshold = newThreshold ?? Math.min(this.threshold, targetSignerCommitments.length);

    if (targetThreshold < 1 || targetThreshold > targetSignerCommitments.length) {
      throw new Error(
        `Invalid threshold ${targetThreshold}. Must be between 1 and ${targetSignerCommitments.length}`
      );
    }

    const { request, salt } = await buildUpdateSignersTransactionRequest(
      this.webClient,
      targetThreshold,
      targetSignerCommitments,
    );

    const summary = await executeForSummary(this.webClient, this._accountId, request);
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
   * Create a "change threshold" proposal.
   *
   * @param newThreshold - The new threshold value
   * @param nonce - Optional proposal nonce (defaults to Date.now())
   */
  async createChangeThresholdProposal(
    newThreshold: number,
    nonce?: number,
  ): Promise<Proposal> {
    if (newThreshold < 1 || newThreshold > this.signerCommitments.length) {
      throw new Error(
        `Invalid threshold ${newThreshold}. Must be between 1 and ${this.signerCommitments.length}`
      );
    }

    if (newThreshold === this.threshold) {
      throw new Error('New threshold is the same as current threshold');
    }

    const { request, salt } = await buildUpdateSignersTransactionRequest(
      this.webClient,
      newThreshold,
      this.signerCommitments,
    );

    const summary = await executeForSummary(this.webClient, this._accountId, request);
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
   * @param newPsmEndpoint - The new PSM server endpoint URL
   * @param newPsmPubkey - The new PSM server's public key commitment (hex)
   * @param nonce - Optional proposal nonce (defaults to Date.now())
   */
  async createSwitchPsmProposal(
    newPsmEndpoint: string,
    newPsmPubkey: string,
    nonce?: number,
  ): Promise<Proposal> {
    const { request, salt } = await buildUpdatePsmTransactionRequest(
      this.webClient,
      newPsmPubkey,
    );

    const summary = await executeForSummary(this.webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'switch_psm',
      saltHex: salt.toHex(),
      newPsmPubkey,
      newPsmEndpoint,
      description: `Switch PSM to ${newPsmEndpoint}`,
    };

    const proposalId = computeCommitmentFromTxSummary(summaryBase64);
    const proposal: Proposal = {
      id: proposalId,
      accountId: this._accountId,
      nonce: proposalNonce,
      status: { type: 'pending', signaturesCollected: 0, signaturesRequired: this.threshold, signers: [] },
      txSummary: summaryBase64,
      signatures: [],
      metadata,
    };

    this.proposals.set(proposal.id, proposal);
    return proposal;
  }

  /**
   * Create a "consume notes" proposal to consume notes sent to the multisig account.
   *
   * @param noteIds - IDs of the notes to consume (hex strings)
   * @param nonce - Optional proposal nonce (defaults to Date.now())
   */
  async createConsumeNotesProposal(
    noteIds: string[],
    nonce?: number,
  ): Promise<Proposal> {
    if (noteIds.length === 0) {
      throw new Error('At least one note ID is required');
    }

    const { request, salt } = buildConsumeNotesTransactionRequest(noteIds);

    const summary = await executeForSummary(this.webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'consume_notes',
      noteIds,
      saltHex: salt.toHex(),
      description: `Consume ${noteIds.length} note(s)`,
    };

    return this.createProposal(proposalNonce, summaryBase64, metadata);
  }

  /**
   * Create a P2ID proposal to send funds to another account.
   *
   * @param recipientId - Account ID of the recipient (hex string)
   * @param faucetId - Faucet/token account ID (hex string)
   * @param amount - Amount to send
   * @param nonce - Optional proposal nonce (defaults to Date.now())
   */
  async createP2idProposal(
    recipientId: string,
    faucetId: string,
    amount: bigint,
    nonce?: number,
  ): Promise<Proposal> {
    if (amount <= 0n) {
      throw new Error('Amount must be greater than 0');
    }

    const { request, salt } = buildP2idTransactionRequest(
      this._accountId,
      recipientId,
      faucetId,
      amount,
    );

    const summary = await executeForSummary(this.webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'p2id',
      saltHex: salt.toHex(),
      recipientId,
      faucetId,
      amount: amount.toString(),
      description: `Send ${amount} to ${recipientId.slice(0, 10)}...`,
    };

    return this.createProposal(proposalNonce, summaryBase64, metadata);
  }

  /**
   * Get notes that can be consumed by this multisig account.
   *
   * Returns a list of notes that are committed on-chain and can be consumed
   * immediately by the multisig account.
   */
  async getConsumableNotes(): Promise<ConsumableNote[]> {
    const accountId = AccountId.fromHex(this._accountId);

    // Get consumable notes for this account
    const consumableRecords = await this.webClient.getConsumableNotes(accountId);

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
    const existingProposal = this.proposals.get(proposalId);

    const signatureHex = this.signer.signCommitment(proposalId);

    const signature: FalconSignature = {
      scheme: 'falcon',
      signature: signatureHex,
    };

    const delta = await this.psm.signDeltaProposal({
      accountId: this._accountId,
      commitment: proposalId,
      signature,
    });

    const proposal = this.deltaToProposal(delta, proposalId, undefined, existingProposal?.signatures);

    if (existingProposal?.metadata) {
      proposal.metadata = existingProposal.metadata;
    }

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

    const proposalType = proposal.metadata?.proposalType;
    const effectiveThreshold = proposalType
      ? this.getEffectiveThreshold(proposalType)
      : this.threshold;

    if (proposal.signatures.length < effectiveThreshold) {
      throw new Error('Proposal is not ready for execution. Still pending signatures.');
    }

    const isSwitchPsm = proposalType === 'switch_psm';

    let txSummaryBase64: string;
    let delta: DeltaObject | undefined;

    if (isSwitchPsm) {
      txSummaryBase64 = proposal.txSummary;
    } else {
      const deltas = await this.psm.getDeltaProposals(this._accountId);
      delta = deltas.find(
        (d) => computeCommitmentFromTxSummary(d.deltaPayload.txSummary.data) === proposalId
      );

      if (!delta) {
        throw new Error(`Proposal not found on server: ${proposalId}`);
      }
      txSummaryBase64 = delta.deltaPayload.txSummary.data;
    }

    const txSummaryBytes = base64ToUint8Array(txSummaryBase64);
    const txSummary = TransactionSummary.deserialize(txSummaryBytes);
    const saltHex = txSummary.salt().toHex();
    const txCommitmentHex = txSummary.toCommitment().toHex();

    const adviceMap = new AdviceMap();

    for (const cosignerSig of proposal.signatures) {
      const signerCommitment = Word.fromHex(normalizeHexWord(cosignerSig.signerId));
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

    if (!isSwitchPsm && delta) {
      const executionDelta = {
        ...delta,
        deltaPayload: delta.deltaPayload.txSummary,
      };

      const pushResult = await this.psm.pushDelta(executionDelta);
      const ackSigHex = pushResult.ackSig;
      if (!ackSigHex) {
        throw new Error('PSM did not return acknowledgment signature');
      }

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
    }

    const metadata = proposal.metadata;
    if (!metadata) {
      throw new Error('Proposal missing metadata');
    }

    let finalRequest: TransactionRequest;
    switch (metadata.proposalType) {
      case 'consume_notes': {
        if (!metadata.noteIds || metadata.noteIds.length === 0) {
          throw new Error('Proposal missing noteIds. Was it created with createConsumeNotesProposal?');
        }
        const { request } = buildConsumeNotesTransactionRequest(
          metadata.noteIds,
          { salt: Word.fromHex(normalizeHexWord(saltHex)), signatureAdviceMap: adviceMap },
        );
        finalRequest = request;
        break;
      }
      case 'switch_psm': {
        if (!metadata.newPsmPubkey) {
          throw new Error('Proposal missing newPsmPubkey. Was it created with createSwitchPsmProposal?');
        }
        const { request } = await buildUpdatePsmTransactionRequest(
          this.webClient,
          metadata.newPsmPubkey,
          { salt: Word.fromHex(normalizeHexWord(saltHex)), signatureAdviceMap: adviceMap },
        );
        finalRequest = request;
        break;
      }
      case 'p2id': {
        if (!metadata.recipientId || !metadata.faucetId || !metadata.amount) {
          throw new Error('Proposal missing P2ID metadata (recipientId, faucetId, amount). Was it created with createP2idProposal?');
        }
        const { request } = buildP2idTransactionRequest(
          this._accountId,
          metadata.recipientId,
          metadata.faucetId,
          BigInt(metadata.amount),
          { salt: Word.fromHex(normalizeHexWord(saltHex)), signatureAdviceMap: adviceMap },
        );
        finalRequest = request;
        break;
      }
      case 'unknown': {
        throw new Error('Cannot execute proposal with unknown type. The proposal must have been imported without proper metadata.');
      }
      default: {
        const { request } = await buildUpdateSignersTransactionRequest(
          this.webClient,
          metadata.targetThreshold,
          metadata.targetSignerCommitments,
          { salt: Word.fromHex(normalizeHexWord(saltHex)), signatureAdviceMap: adviceMap },
        );
        finalRequest = request;
        break;
      }
    }

    const accountId = AccountId.fromHex(this._accountId);
    const result = await this.webClient.executeTransaction(accountId, finalRequest);
    const proven = await this.webClient.proveTransaction(result, null);
    const submissionHeight = await this.webClient.submitProvenTransaction(proven, result);
    await this.webClient.applyTransaction(result, submissionHeight);

    proposal.status = { type: 'finalized' };
  }

  /**
   * Export a proposal for offline signing
   */
  async exportProposal(proposalId: string): Promise<ExportedProposal> {
    const deltas = await this.psm.getDeltaProposals(this._accountId);
    const delta = deltas.find((d) => computeCommitmentFromTxSummary(d.deltaPayload.txSummary.data) === proposalId);

    if (!delta) {
      throw new Error(`Proposal not found: ${proposalId}`);
    }

    const signatures =
      delta.status.status === 'pending'
        ? delta.status.cosignerSigs.map((s) => ({
            commitment: s.signerId,
            signatureHex: s.signature.signature,
          }))
        : [];

    return {
      accountId: delta.accountId,
      nonce: delta.nonce,
      commitment: proposalId,
      txSummaryBase64: delta.deltaPayload.txSummary.data,
      signatures,
    };
  }

  /**
   * Export a proposal to JSON for side-channel sharing.
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
   *
   * @param json - JSON string from exportProposalToJson
   * @returns The imported proposal
   */
  importProposal(json: string): Proposal {
    const exported: ExportedProposal = JSON.parse(json);

    if (!exported.accountId || !exported.txSummaryBase64 || !exported.commitment) {
      throw new Error('Invalid proposal JSON: missing required fields');
    }

    if (exported.accountId.toLowerCase() !== this._accountId.toLowerCase()) {
      throw new Error(`Proposal is for a different account: ${exported.accountId}`);
    }

    const computedCommitment = computeCommitmentFromTxSummary(exported.txSummaryBase64);
    if (computedCommitment !== exported.commitment) {
      throw new Error('Invalid proposal: commitment does not match tx_summary');
    }

    const metadata: ProposalMetadata = (exported.metadata as ProposalMetadata) ?? {
      proposalType: 'unknown',
      description: '',
    };

    const signaturesCollected = exported.signatures.length;
    const signaturesRequired = this.getEffectiveThreshold(metadata.proposalType);
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
      metadata,
    };

    this.proposals.set(proposal.id, proposal);

    return proposal;
  }

  /**
   * Sign an imported proposal and return updated JSON for sharing..
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
    const proposalType = proposal.metadata?.proposalType;
    const effectiveThreshold = proposalType
      ? this.getEffectiveThreshold(proposalType)
      : this.threshold;

    if (signaturesCollected >= effectiveThreshold) {
      proposal.status = { type: 'ready' };
    } else if (proposal.status.type === 'pending') {
      proposal.status = {
        type: 'pending',
        signaturesCollected,
        signaturesRequired: effectiveThreshold,
        signers: proposal.signatures.map((s) => s.signerId),
      };
    }

    // Return updated JSON
    return this.exportProposalToJson(proposalId);
  }

  private deltaToProposal(
    delta: DeltaObject,
    proposalId: string,
    metadata?: ProposalMetadata,
    existingSignatures?: ProposalSignatureEntry[],
  ): Proposal {
    const resolvedMetadata: ProposalMetadata | undefined =
      metadata ??
      (delta.deltaPayload.metadata ? this.fromPsmMetadata(delta.deltaPayload.metadata) : undefined);
    if (!resolvedMetadata) {
      throw new Error('Missing proposal metadata');
    }

    const status = this.deltaStatusToProposalStatus(delta.status, resolvedMetadata.proposalType);

    const signaturesFromStatus =
      delta.status.status === 'pending'
        ? delta.status.cosignerSigs.map((s) => ({
            signerId: s.signerId,
            signature: s.signature,
            timestamp: s.timestamp,
          }))
        : [];

    const signaturesMap = new Map<string, ProposalSignatureEntry>();
    for (const sig of existingSignatures ?? []) {
      signaturesMap.set(sig.signerId, sig);
    }
    for (const sig of signaturesFromStatus) {
      signaturesMap.set(sig.signerId, sig);
    }
    const signatures = Array.from(signaturesMap.values());

    return {
      id: proposalId,
      accountId: delta.accountId,
      nonce: delta.nonce,
      status,
      txSummary: delta.deltaPayload.txSummary.data,
      signatures,
      metadata: resolvedMetadata,
    };
  }

  private buildPsmMetadata(metadata: ProposalMetadata): PsmProposalMetadata {
    const base: PsmProposalMetadata = {
      proposalType: metadata.proposalType,
      description: metadata.description,
      salt: metadata.saltHex,
    };

    switch (metadata.proposalType) {
      case 'consume_notes':
        return {
          ...base,
          noteIds: metadata.noteIds,
        };
      case 'p2id':
        return {
          ...base,
          recipientId: metadata.recipientId,
          faucetId: metadata.faucetId,
          amount: metadata.amount,
        };
      case 'switch_psm':
        return {
          ...base,
          targetThreshold: metadata.targetThreshold,
          signerCommitments: metadata.targetSignerCommitments,
          newPsmPubkey: metadata.newPsmPubkey,
          newPsmEndpoint: metadata.newPsmEndpoint,
        };
      case 'add_signer':
      case 'remove_signer':
      case 'change_threshold':
        return {
          ...base,
          targetThreshold: metadata.targetThreshold,
          signerCommitments: metadata.targetSignerCommitments,
        };
      case 'unknown':
        return base;
    }
  }

  private fromPsmMetadata(psm: PsmProposalMetadata): ProposalMetadata | undefined {
    if (!psm.proposalType) return undefined;
    const base = {
      description: psm.description ?? '',
      saltHex: psm.salt,
    };

    switch (psm.proposalType) {
      case 'p2id':
        return {
          ...base,
          proposalType: 'p2id',
          recipientId: psm.recipientId ?? '',
          faucetId: psm.faucetId ?? '',
          amount: psm.amount ?? '0',
        };
      case 'consume_notes':
        return {
          ...base,
          proposalType: 'consume_notes',
          noteIds: psm.noteIds ?? [],
        };
      case 'switch_psm':
        return {
          ...base,
          proposalType: 'switch_psm',
          newPsmPubkey: psm.newPsmPubkey ?? '',
          newPsmEndpoint: psm.newPsmEndpoint,
          targetThreshold: psm.targetThreshold,
          targetSignerCommitments: psm.signerCommitments,
        };
      case 'add_signer':
      case 'remove_signer':
      case 'change_threshold':
        return {
          ...base,
          proposalType: psm.proposalType,
          targetThreshold: psm.targetThreshold ?? 0,
          targetSignerCommitments: psm.signerCommitments ?? [],
        };
      default:
        return undefined;
    }
  }

  private deltaStatusToProposalStatus(status: DeltaStatus, proposalType?: ProposalType): ProposalStatus {
    switch (status.status) {
      case 'pending': {
        const signaturesCollected = status.cosignerSigs.length;
        const signaturesRequired = proposalType
          ? this.getEffectiveThreshold(proposalType)
          : this.threshold;
        if (signaturesCollected >= signaturesRequired) {
          return { type: 'ready' };
        }
        return {
          type: 'pending',
          signaturesCollected,
          signaturesRequired,
          signers: status.cosignerSigs.map((s) => s.signerId),
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
