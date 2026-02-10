import { PsmHttpClient, type Signer, type AuthConfig, type StateObject } from '@openzeppelin/psm-client';
import type {
  ConsumableNote,
  ExportedTransactionProposal,
  MultisigConfig,
  NoteAsset,
  TransactionProposal,
  ProposalMetadata,
  TransactionProposalStatus,
  ProposalType,
  SignTransactionProposalParams,
  SyncResult,
  TransactionProposalResult,
} from './types.js';
import type { ProcedureName } from './procedures.js';
import { AccountInspector, type DetectedMultisigConfig } from './inspector.js';
import type { WebClient, TransactionRequest } from '@miden-sdk/miden-sdk';
import {
  Account,
  AccountId,
} from '@miden-sdk/miden-sdk';
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
import { signatureHexToBytes } from './utils/signature.js';
import { computeCommitmentFromTxSummary, accountIdToHex } from './multisig/helpers.js';
import {
  buildPsmMetadata,
  deltaToProposal,
  resolveMetadata,
  signatureRequirementForProposal,
} from './multisig/proposal/parser.js';
import {
  toPsmSignature,
  buildPsmSignatureFromSigner,
} from './multisig/signing.js';
import { executeProposalWorkflow } from './multisig/proposal/execution.js';

export interface AccountState {
  accountId: string;
  commitment: string;
  stateDataBase64: string;
  createdAt: string;
  updatedAt: string;
  authScheme?: string;
}

export class Multisig {
  account: Account | null;
  threshold: number;
  signerCommitments: string[];
  psmCommitment: string;
  psmPublicKey?: string;
  procedureThresholds: Map<ProcedureName, number>;
  readonly signatureScheme: Signer['scheme'];

  private psm: PsmHttpClient;
  private readonly signer: Signer;
  private readonly webClient: WebClient;
  private readonly _accountId: string;
  private proposals: Map<string, TransactionProposal> = new Map();

  constructor(
    account: Account | null,
    config: MultisigConfig,
    psm: PsmHttpClient,
    signer: Signer,
    webClient: WebClient,
    accountId?: string
  ) {
    if (config.signatureScheme && config.signatureScheme !== signer.scheme) {
      throw new Error(
        `signature scheme mismatch: config=${config.signatureScheme} signer=${signer.scheme}`
      );
    }
    this.account = account;
    this.threshold = config.threshold;
    this.signerCommitments = config.signerCommitments;
    this.psmCommitment = config.psmCommitment;
    this.psmPublicKey = config.psmPublicKey;
    this.signatureScheme = config.signatureScheme ?? signer.scheme;
    this.procedureThresholds = new Map(
      (config.procedureThresholds ?? []).map((pt) => [pt.procedure, pt.threshold])
    );
    this.psm = psm;
    this.signer = signer;
    this.webClient = webClient;
    this._accountId = accountId ?? (account ? accountIdToHex(account) : '');
  }

  get accountId(): string {
    return this._accountId;
  }

  get signerCommitment(): string {
    return this.signer.commitment;
  }

  setPsmPublicKey(pubkey?: string): void {
    this.psmPublicKey = pubkey;
  }

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

  setPsmClient(psmClient: PsmHttpClient): void {
    this.psm = psmClient;
    this.psm.setSigner(this.signer);
  }

  async fetchState(): Promise<AccountState> {
    const state: StateObject = await this.psm.getState(this._accountId);

    return {
      accountId: state.accountId,
      commitment: state.commitment,
      stateDataBase64: state.stateJson.data,
      createdAt: state.createdAt,
      updatedAt: state.updatedAt,
      authScheme: state.authScheme,
    };
  }

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

  async registerOnPsm(initialStateBase64?: string): Promise<void> {
    if (!this.account && !initialStateBase64) {
      throw new Error('Cannot register on PSM: no account available and no initial state provided');
    }

    let stateData: string;
    if (initialStateBase64) {
      stateData = initialStateBase64;
    } else {
      const accountBytes: Uint8Array = this.account!.serialize();
      stateData = uint8ArrayToBase64(accountBytes);
    }

    const auth: AuthConfig = this.signer.scheme === 'ecdsa'
      ? { MidenEcdsa: { cosigner_commitments: this.signerCommitments } }
      : { MidenFalconRpo: { cosigner_commitments: this.signerCommitments } };

    const response = await this.psm.configure({
      accountId: this._accountId,
      auth,
      initialState: { data: stateData, accountId: this._accountId },
    });

    if (!response.success) {
      throw new Error(`Failed to register on PSM: ${response.message}`);
    }

    if (response.ackCommitment && this.psmCommitment) {
      const onChain = normalizeHexWord(this.psmCommitment);
      const server = normalizeHexWord(response.ackCommitment);
      if (onChain !== server) {
        throw new Error(
          `PSM commitment mismatch: on-chain=${onChain}, server=${server}`
        );
      }
    }
  }

  async syncAll(): Promise<SyncResult> {
    const proposals = await this.syncTransactionProposals();
    const state = await this.syncState();
    const notes = await this.getConsumableNotes();
    const config = AccountInspector.fromBase64(state.stateDataBase64, this.signatureScheme);

    this.threshold = config.threshold;
    this.signerCommitments = config.signerCommitments;
    if (config.psmCommitment) {
      this.psmCommitment = config.psmCommitment;
    }
    this.procedureThresholds = config.procedureThresholds;

    return { proposals, state, notes, config };
  }

  async getAccountConfig(): Promise<DetectedMultisigConfig> {
    const state = await this.syncState();
    return AccountInspector.fromBase64(state.stateDataBase64, this.signatureScheme);
  }

  async switchPsm(psmClient: PsmHttpClient): Promise<void> {
    const accountId = AccountId.fromHex(this._accountId);
    const localAccount = await this.webClient.getAccount(accountId);
    if (localAccount) {
      this.account = localAccount;
      const config = AccountInspector.fromAccount(localAccount, this.signatureScheme);
      if (config.psmCommitment) {
        this.psmCommitment = config.psmCommitment;
      }
      this.threshold = config.threshold;
      this.signerCommitments = config.signerCommitments;
      this.procedureThresholds = config.procedureThresholds;
    }

    this.setPsmClient(psmClient);

    const accountBytes: Uint8Array = this.account!.serialize();
    const stateData = uint8ArrayToBase64(accountBytes);

    const auth: AuthConfig = this.signer.scheme === 'ecdsa'
      ? { MidenEcdsa: { cosigner_commitments: this.signerCommitments } }
      : { MidenFalconRpo: { cosigner_commitments: this.signerCommitments } };

    const response = await this.psm.configure({
      accountId: this._accountId,
      auth,
      initialState: { data: stateData, accountId: this._accountId },
    });

    if (!response.success) {
      throw new Error(`Failed to register on PSM: ${response.message}`);
    }

    if (response.ackCommitment) {
      this.psmCommitment = normalizeHexWord(response.ackCommitment);
    }
    if (response.ackPubkey) {
      this.psmPublicKey = response.ackPubkey;
    }
  }

  async syncTransactionProposals(): Promise<TransactionProposal[]> {
    const deltas = await this.psm.getDeltaProposals(this._accountId);

    const serverProposalIds = new Set<string>();
    for (const delta of deltas) {
      const proposalId = computeCommitmentFromTxSummary(delta.deltaPayload.txSummary.data);
      serverProposalIds.add(proposalId);
      const existingProposal = this.proposals.get(proposalId);

      const resolvedMetadata = resolveMetadata(delta, existingProposal?.metadata);
      if (!resolvedMetadata) {
        throw new Error('Missing proposal metadata from PSM');
      }

      const proposal = deltaToProposal({
        delta,
        proposalId,
        metadata: resolvedMetadata,
        signaturesRequired: signatureRequirementForProposal(
          resolvedMetadata,
          this.threshold,
          (proposalType) => this.getEffectiveThreshold(proposalType),
        ),
        existingSignatures: existingProposal?.signatures,
      });

      this.proposals.set(proposal.id, proposal);
    }

    for (const [id, proposal] of this.proposals) {
      if (serverProposalIds.has(id)) continue;
      const isLocalOnly = proposal.metadata?.proposalType === 'switch_psm' && proposal.status.type !== 'finalized';
      if (!isLocalOnly) {
        this.proposals.delete(id);
      }
    }

    return Array.from(this.proposals.values());
  }

  listTransactionProposals(): TransactionProposal[] {
    return Array.from(this.proposals.values());
  }

  async createProposal(nonce: number, txSummaryBase64: string, metadata: ProposalMetadata): Promise<TransactionProposal> {
    const psmMetadata = buildPsmMetadata(metadata);

    const response = await this.psm.pushDeltaProposal({
      accountId: this._accountId,
      nonce,
      deltaPayload: {
        txSummary: { data: txSummaryBase64 },
        signatures: [],
        metadata: psmMetadata,
      },
    });

    const proposal = deltaToProposal({
      delta: response.delta,
      proposalId: response.commitment,
      metadata,
      signaturesRequired: signatureRequirementForProposal(
        metadata,
        this.threshold,
        (proposalType) => this.getEffectiveThreshold(proposalType),
      ),
    });
    this.proposals.set(proposal.id, proposal);

    return proposal;
  }

  async createAddSignerProposal(
    newCommitment: string,
    options?: { nonce?: number; newThreshold?: number },
  ): Promise<TransactionProposalResult> {
    const targetThreshold = options?.newThreshold ?? this.threshold;
    const targetSignerCommitments = [...this.signerCommitments, newCommitment];

    const { request, salt } = await buildUpdateSignersTransactionRequest(
      this.webClient,
      targetThreshold,
      targetSignerCommitments,
      { signatureScheme: this.signatureScheme }
    );

    const metadata: ProposalMetadata = {
      proposalType: 'add_signer',
      targetThreshold,
      targetSignerCommitments,
      saltHex: salt.toHex(),
      description: `Add signer ${newCommitment.slice(0, 10)}...`,
    };
    return this.createAndSyncFromRequest(request, metadata, options?.nonce);
  }

  async createRemoveSignerProposal(
    signerToRemove: string,
    options?: { nonce?: number; newThreshold?: number },
  ): Promise<TransactionProposalResult> {
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

    const targetThreshold = options?.newThreshold ?? Math.min(this.threshold, targetSignerCommitments.length);

    if (targetThreshold < 1 || targetThreshold > targetSignerCommitments.length) {
      throw new Error(
        `Invalid threshold ${targetThreshold}. Must be between 1 and ${targetSignerCommitments.length}`
      );
    }

    const { request, salt } = await buildUpdateSignersTransactionRequest(
      this.webClient,
      targetThreshold,
      targetSignerCommitments,
      { signatureScheme: this.signatureScheme }
    );

    const metadata: ProposalMetadata = {
      proposalType: 'remove_signer',
      targetThreshold,
      targetSignerCommitments,
      saltHex: salt.toHex(),
      description: `Remove signer ${signerToRemove.slice(0, 10)}...`,
    };
    return this.createAndSyncFromRequest(request, metadata, options?.nonce);
  }

  async createChangeThresholdProposal(
    newThreshold: number,
    options?: { nonce?: number },
  ): Promise<TransactionProposalResult> {
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
      { signatureScheme: this.signatureScheme }
    );

    const metadata: ProposalMetadata = {
      proposalType: 'change_threshold',
      targetThreshold: newThreshold,
      targetSignerCommitments: this.signerCommitments,
      saltHex: salt.toHex(),
      description: `Change threshold from ${this.threshold} to ${newThreshold}`,
    };
    return this.createAndSyncFromRequest(request, metadata, options?.nonce);
  }

  async createSwitchPsmProposal(
    newPsmEndpoint: string,
    newPsmPubkey: string,
    options?: { nonce?: number },
  ): Promise<TransactionProposalResult> {
    const { request, salt } = await buildUpdatePsmTransactionRequest(
      this.webClient,
      newPsmPubkey,
      { signatureScheme: this.signatureScheme }
    );

    const summary = await executeForSummary(this.webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = options?.nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'switch_psm',
      saltHex: salt.toHex(),
      newPsmPubkey,
      newPsmEndpoint,
      description: `Switch PSM to ${newPsmEndpoint}`,
    };

    const proposalId = computeCommitmentFromTxSummary(summaryBase64);
    const proposal: TransactionProposal = {
      id: proposalId,
      commitment: proposalId,
      accountId: this._accountId,
      nonce: proposalNonce,
      status: { type: 'pending', signaturesCollected: 0, signaturesRequired: this.threshold, signers: [] },
      txSummary: summaryBase64,
      signatures: [],
      metadata,
    };

    this.proposals.set(proposal.id, proposal);
    const proposals = this.listTransactionProposals();
    return { proposal, proposals };
  }

  async createConsumeNotesProposal(
    noteIds: string[],
    options?: { nonce?: number },
  ): Promise<TransactionProposalResult> {
    if (noteIds.length === 0) {
      throw new Error('At least one note ID is required');
    }

    const { request, salt } = await buildConsumeNotesTransactionRequest(this.webClient, noteIds);

    const metadata: ProposalMetadata = {
      proposalType: 'consume_notes',
      noteIds,
      saltHex: salt.toHex(),
      description: `Consume ${noteIds.length} note(s)`,
    };
    return this.createAndSyncFromRequest(request, metadata, options?.nonce);
  }

  async createSendProposal(
    recipientId: string,
    faucetId: string,
    amount: bigint,
    options?: { nonce?: number },
  ): Promise<TransactionProposalResult> {
    if (amount <= 0n) {
      throw new Error('Amount must be greater than 0');
    }

    const { request, salt } = buildP2idTransactionRequest(
      this._accountId,
      recipientId,
      faucetId,
      amount,
    );

    const metadata: ProposalMetadata = {
      proposalType: 'p2id',
      saltHex: salt.toHex(),
      recipientId,
      faucetId,
      amount: amount.toString(),
      description: `Send ${amount} to ${recipientId.slice(0, 10)}...`,
    };
    return this.createAndSyncFromRequest(request, metadata, options?.nonce);
  }

  async getConsumableNotes(): Promise<ConsumableNote[]> {
    const accountId = AccountId.fromHex(this._accountId);

    const consumableRecords = await this.webClient.getConsumableNotes(accountId);
    const notes: ConsumableNote[] = [];
    for (const record of consumableRecords) {
      const inputNote = record.inputNoteRecord();
      const consumability = record.noteConsumability();

      const canConsumeNow = consumability.some(
        (c) => c.accountId().toString().toLowerCase() === this._accountId.toLowerCase() &&
               c.consumptionStatus().consumableAfterBlock() === undefined
      );

      if (canConsumeNow) {
        const noteId = inputNote.id().toString();
        const details = inputNote.details();
        const fungibleAssets = details.assets().fungibleAssets();

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

  async signTransactionProposal(commitment: string): Promise<TransactionProposal[]> {
    const existingProposal = this.proposals.get(commitment);
    const signature = await buildPsmSignatureFromSigner(this.signer, commitment);

    const delta = await this.psm.signDeltaProposal({
      accountId: this._accountId,
      commitment,
      signature,
    });

    const resolvedMetadata = resolveMetadata(delta, existingProposal?.metadata);
    if (!resolvedMetadata) {
      throw new Error('Missing proposal metadata');
    }
    const proposal = deltaToProposal({
      delta,
      proposalId: commitment,
      metadata: resolvedMetadata,
      signaturesRequired: signatureRequirementForProposal(
        resolvedMetadata,
        this.threshold,
        (proposalType) => this.getEffectiveThreshold(proposalType),
      ),
      existingSignatures: existingProposal?.signatures,
    });

    this.proposals.set(proposal.id, proposal);

    return this.listTransactionProposals();
  }

  async signTransactionProposalExternal(
    params: SignTransactionProposalParams,
  ): Promise<TransactionProposal[]> {
    const { commitment, signature: signatureHex, publicKey, scheme } = params;
    const resolvedScheme = scheme ?? this.signatureScheme;

    const existingProposal = this.proposals.get(commitment);
    const signature = toPsmSignature(resolvedScheme, signatureHex, publicKey);

    const delta = await this.psm.signDeltaProposal({
      accountId: this._accountId,
      commitment,
      signature,
    });

    const resolvedMetadata = resolveMetadata(delta, existingProposal?.metadata);
    if (!resolvedMetadata) {
      throw new Error('Missing proposal metadata');
    }
    const proposal = deltaToProposal({
      delta,
      proposalId: commitment,
      metadata: resolvedMetadata,
      signaturesRequired: signatureRequirementForProposal(
        resolvedMetadata,
        this.threshold,
        (proposalType) => this.getEffectiveThreshold(proposalType),
      ),
      existingSignatures: existingProposal?.signatures,
    });

    this.proposals.set(proposal.id, proposal);

    return this.listTransactionProposals();
  }

  async executeTransactionProposal(commitment: string): Promise<void> {
    const proposal = this.proposals.get(commitment);
    if (!proposal) {
      throw new Error(`Proposal not found: ${commitment}`);
    }

    await executeProposalWorkflow({
      proposal,
      accountId: this._accountId,
      threshold: this.threshold,
      signerCommitments: this.signerCommitments,
      psmCommitment: this.psmCommitment,
      psmPublicKey: this.psmPublicKey,
      signatureScheme: this.signatureScheme,
      getEffectiveThreshold: (proposalType) => this.getEffectiveThreshold(proposalType),
      psm: this.psm,
      webClient: this.webClient,
    });

    proposal.status = { type: 'finalized' };
  }

  exportTransactionProposalToJson(commitment: string): string {
    const proposal = this.proposals.get(commitment);
    if (!proposal) {
      throw new Error(`Proposal not found in local cache: ${commitment}`);
    }

    const exported: ExportedTransactionProposal = {
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

  importTransactionProposal(json: string): TransactionProposalResult {
    const exported: ExportedTransactionProposal = JSON.parse(json);

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
    const status: TransactionProposalStatus = signaturesCollected >= signaturesRequired
      ? { type: 'ready' }
      : {
          type: 'pending',
          signaturesCollected,
          signaturesRequired,
          signers: exported.signatures.map((s) => s.commitment),
        };

    const proposal: TransactionProposal = {
      id: exported.commitment,
      commitment: exported.commitment,
      accountId: exported.accountId,
      nonce: exported.nonce,
      status,
      txSummary: exported.txSummaryBase64,
      signatures: exported.signatures.map((s) => ({
        signerId: s.commitment,
        signature: { scheme: this.signer.scheme, signature: s.signatureHex },
        timestamp: s.timestamp || new Date().toISOString(),
      })),
      metadata,
    };

    this.proposals.set(proposal.id, proposal);
    const proposals = this.listTransactionProposals();
    return { proposal, proposals };
  }

  async signTransactionProposalOffline(commitment: string): Promise<string> {
    const proposal = this.proposals.get(commitment);
    if (!proposal) {
      throw new Error(`Proposal not found: ${commitment}`);
    }

    const alreadySigned = proposal.signatures.some(
      (s) => s.signerId.toLowerCase() === this.signer.commitment.toLowerCase()
    );
    if (alreadySigned) {
      throw new Error('You have already signed this proposal');
    }

    const signatureHex = await this.signer.signCommitment(commitment);

    const sigEntry = toPsmSignature(this.signer.scheme, signatureHex, this.signer.publicKey);
    proposal.signatures.push({
      signerId: this.signer.commitment,
      signature: sigEntry,
      timestamp: new Date().toISOString(),
    });

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

    return this.exportTransactionProposalToJson(commitment);
  }

  private async createAndSyncFromRequest(
    request: TransactionRequest,
    metadata: ProposalMetadata,
    nonce?: number,
  ): Promise<TransactionProposalResult> {
    const summary = await executeForSummary(this.webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = nonce ?? Date.now();
    const proposal = await this.createProposal(proposalNonce, summaryBase64, metadata);
    return { proposal, proposals: this.listTransactionProposals() };
  }

}
