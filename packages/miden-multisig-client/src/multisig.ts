import { PsmHttpClient, type DeltaObject, type DeltaStatus, type ProposalSignature, type Signer, type AuthConfig, type StateObject, type ProposalMetadata as PsmProposalMetadata } from '@openzeppelin/psm-client';
import type {
  ConsumableNote,
  ExportedTransactionProposal,
  MultisigConfig,
  NoteAsset,
  TransactionProposal,
  ProposalMetadata,
  TransactionProposalSignature,
  TransactionProposalStatus,
  ProposalType,
  SignTransactionProposalParams,
  SyncResult,
  TransactionProposalResult,
} from './types.js';
import type { ProcedureName } from './procedures.js';
import { AccountInspector, type DetectedMultisigConfig } from './inspector.js';
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
import { buildSignatureAdviceEntry, signatureHexToBytes, tryComputeEcdsaCommitmentHex } from './utils/signature.js';
import { computeCommitmentFromTxSummary, accountIdToHex } from './multisig/helpers.js';

export interface AccountState {
  accountId: string;
  commitment: string;
  stateDataBase64: string;
  createdAt: string;
  updatedAt: string;
  authScheme?: string;
}

export class Multisig {
  readonly account: Account | null;
  readonly threshold: number;
  readonly signerCommitments: string[];
  readonly psmCommitment: string;
  psmPublicKey?: string;
  readonly procedureThresholds: Map<ProcedureName, number>;
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
          `PSM commitment mismatch: on-chain=${onChain}, server=${server}. ` +
          `Re-create the account with getPubkey('${this.signatureScheme}') to get the correct PSM commitment.`
        );
      }
    }
  }

  async syncAll(): Promise<SyncResult> {
    const proposals = await this.syncTransactionProposals();
    const state = await this.syncState();
    const notes = await this.getConsumableNotes();
    const config = AccountInspector.fromBase64(state.stateDataBase64, this.signatureScheme);
    return { proposals, state, notes, config };
  }

  async getAccountConfig(): Promise<DetectedMultisigConfig> {
    const state = await this.syncState();
    return AccountInspector.fromBase64(state.stateDataBase64, this.signatureScheme);
  }

  async switchPsm(psmClient: PsmHttpClient): Promise<void> {
    this.setPsmClient(psmClient);
    const state = await this.fetchState();
    await this.registerOnPsm(state.stateDataBase64);
  }

  async syncTransactionProposals(): Promise<TransactionProposal[]> {
    const deltas = await this.psm.getDeltaProposals(this._accountId);

    const serverProposalIds = new Set<string>();
    for (const delta of deltas) {
      const proposalId = computeCommitmentFromTxSummary(delta.deltaPayload.txSummary.data);
      serverProposalIds.add(proposalId);
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

    const summary = await executeForSummary(this.webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = options?.nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'add_signer',
      targetThreshold,
      targetSignerCommitments,
      saltHex: salt.toHex(),
      description: `Add signer ${newCommitment.slice(0, 10)}...`,
    };

    const proposal = await this.createProposal(proposalNonce, summaryBase64, metadata);
    return this.syncAfterCreate(proposal);
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

    const summary = await executeForSummary(this.webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = options?.nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'remove_signer',
      targetThreshold,
      targetSignerCommitments,
      saltHex: salt.toHex(),
      description: `Remove signer ${signerToRemove.slice(0, 10)}...`,
    };

    const proposal = await this.createProposal(proposalNonce, summaryBase64, metadata);
    return this.syncAfterCreate(proposal);
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

    const summary = await executeForSummary(this.webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = options?.nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'change_threshold',
      targetThreshold: newThreshold,
      targetSignerCommitments: this.signerCommitments,
      saltHex: salt.toHex(),
      description: `Change threshold from ${this.threshold} to ${newThreshold}`,
    };

    const proposal = await this.createProposal(proposalNonce, summaryBase64, metadata);
    return this.syncAfterCreate(proposal);
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

    const { request, salt } = buildConsumeNotesTransactionRequest(noteIds);

    const summary = await executeForSummary(this.webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = options?.nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'consume_notes',
      noteIds,
      saltHex: salt.toHex(),
      description: `Consume ${noteIds.length} note(s)`,
    };

    const proposal = await this.createProposal(proposalNonce, summaryBase64, metadata);
    return this.syncAfterCreate(proposal);
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

    const summary = await executeForSummary(this.webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = options?.nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'p2id',
      saltHex: salt.toHex(),
      recipientId,
      faucetId,
      amount: amount.toString(),
      description: `Send ${amount} to ${recipientId.slice(0, 10)}...`,
    };

    const proposal = await this.createProposal(proposalNonce, summaryBase64, metadata);
    return this.syncAfterCreate(proposal);
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
               c.consumableAfterBlock() === undefined
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

    const signatureHex = this.signer.signCommitment(commitment);

    const signature: ProposalSignature = this.signer.scheme === 'ecdsa'
      ? { scheme: 'ecdsa', signature: signatureHex, publicKey: this.signer.publicKey }
      : { scheme: 'falcon', signature: signatureHex };

    const delta = await this.psm.signDeltaProposal({
      accountId: this._accountId,
      commitment,
      signature,
    });

    const proposal = this.deltaToProposal(delta, commitment, undefined, existingProposal?.signatures);

    if (existingProposal?.metadata) {
      proposal.metadata = existingProposal.metadata;
    }

    this.proposals.set(proposal.id, proposal);

    return this.syncTransactionProposals();
  }

  async signTransactionProposalExternal(
    params: SignTransactionProposalParams,
  ): Promise<TransactionProposal[]> {
    const { commitment, signature: signatureHex, publicKey, scheme } = params;
    const resolvedScheme = scheme ?? this.signatureScheme;

    const existingProposal = this.proposals.get(commitment);

    const signature: ProposalSignature = resolvedScheme === 'ecdsa'
      ? { scheme: 'ecdsa', signature: signatureHex, publicKey: publicKey! }
      : { scheme: 'falcon', signature: signatureHex };

    const delta = await this.psm.signDeltaProposal({
      accountId: this._accountId,
      commitment,
      signature,
    });

    const proposal = this.deltaToProposal(delta, commitment, undefined, existingProposal?.signatures);

    if (existingProposal?.metadata) {
      proposal.metadata = existingProposal.metadata;
    }

    this.proposals.set(proposal.id, proposal);

    return this.syncTransactionProposals();
  }

  async executeTransactionProposal(commitment: string): Promise<void> {
    const proposal = this.proposals.get(commitment);
    if (!proposal) {
      throw new Error(`Proposal not found: ${commitment}`);
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
        (d) => computeCommitmentFromTxSummary(d.deltaPayload.txSummary.data) === commitment
      );

      if (!delta) {
        throw new Error(`Proposal not found on server: ${commitment}`);
      }
      txSummaryBase64 = delta.deltaPayload.txSummary.data;
    }

    const txSummaryBytes = base64ToUint8Array(txSummaryBase64);
    const txSummary = TransactionSummary.deserialize(txSummaryBytes);
    const saltHex = txSummary.salt().toHex();
    const txCommitmentHex = txSummary.toCommitment().toHex();

    const adviceMap = new AdviceMap();
    const normalizedSignerCommitments = new Set(
      this.signerCommitments.map((c) => normalizeHexWord(c))
    );

    for (const cosignerSig of proposal.signatures) {
      let signerCommitmentHex = normalizeHexWord(cosignerSig.signerId);
      if (cosignerSig.signature.scheme === 'ecdsa' && cosignerSig.signature.publicKey) {
        const derived = tryComputeEcdsaCommitmentHex(cosignerSig.signature.publicKey);
        if (derived && derived !== signerCommitmentHex) {
          if (!normalizedSignerCommitments.has(derived)) {
            throw new Error(
              `ECDSA public key commitment mismatch: derived commitment ${derived} is not in signerCommitments.`
            );
          }
          signerCommitmentHex = derived;
        }
      }
      const signerCommitment = Word.fromHex(signerCommitmentHex);
      const sigBytes = signatureHexToBytes(
        cosignerSig.signature.signature,
        cosignerSig.signature.scheme
      );
      const signature = Signature.deserialize(sigBytes);
      const txCommitment = Word.fromHex(normalizeHexWord(txCommitmentHex));

      const isEcdsa = cosignerSig.signature.scheme === 'ecdsa' && cosignerSig.signature.publicKey;
      const { key, values } = buildSignatureAdviceEntry(
        signerCommitment,
        txCommitment,
        signature,
        isEcdsa ? cosignerSig.signature.publicKey : undefined,
        isEcdsa ? cosignerSig.signature.signature : undefined,
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

      const psmAckScheme: 'ecdsa' | 'falcon' = (pushResult.ackScheme as 'ecdsa' | 'falcon') || this.signatureScheme;
      const psmAckPubkey = pushResult.ackPubkey || this.psmPublicKey;
      const psmCommitmentHex = normalizeHexWord(this.psmCommitment);

      if (psmAckScheme === 'ecdsa' && psmAckPubkey) {
        const derived = tryComputeEcdsaCommitmentHex(psmAckPubkey);
        if (derived && derived !== psmCommitmentHex) {
          throw new Error(`PSM public key commitment mismatch`);
        }
      }
      const psmCommitment = Word.fromHex(psmCommitmentHex);
      const ackSigBytes = signatureHexToBytes(ackSigHex, psmAckScheme);
      const ackSignature = Signature.deserialize(ackSigBytes);
      const txCommitmentForAck = Word.fromHex(normalizeHexWord(txCommitmentHex));
      const isAckEcdsa = psmAckScheme === 'ecdsa' && psmAckPubkey;
      const { key: ackKey, values: ackValues } = buildSignatureAdviceEntry(
        psmCommitment,
        txCommitmentForAck,
        ackSignature,
        isAckEcdsa ? psmAckPubkey : undefined,
        isAckEcdsa ? ackSigHex : undefined,
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
          {
            salt: Word.fromHex(normalizeHexWord(saltHex)),
            signatureAdviceMap: adviceMap,
            signatureScheme: this.signatureScheme,
          },
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
          {
            salt: Word.fromHex(normalizeHexWord(saltHex)),
            signatureAdviceMap: adviceMap,
            signatureScheme: this.signatureScheme,
          },
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

  signTransactionProposalOffline(commitment: string): string {
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

    const signatureHex = this.signer.signCommitment(commitment);

    const sigEntry: TransactionProposalSignature['signature'] = this.signer.scheme === 'ecdsa'
      ? { scheme: 'ecdsa', signature: signatureHex, publicKey: this.signer.publicKey }
      : { scheme: 'falcon', signature: signatureHex };
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

  private async syncAfterCreate(proposal: TransactionProposal): Promise<TransactionProposalResult> {
    let proposals: TransactionProposal[];
    try {
      proposals = await this.syncTransactionProposals();
      if (!proposals.find((p) => p.id === proposal.id)) {
        proposals = [...proposals, proposal];
      }
    } catch {
      proposals = this.listTransactionProposals();
    }
    return { proposal, proposals };
  }

  private deltaToProposal(
    delta: DeltaObject,
    proposalId: string,
    metadata?: ProposalMetadata,
    existingSignatures?: TransactionProposalSignature[],
  ): TransactionProposal {
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

    const signaturesMap = new Map<string, TransactionProposalSignature>();
    for (const sig of existingSignatures ?? []) {
      signaturesMap.set(sig.signerId, sig);
    }
    for (const sig of signaturesFromStatus) {
      signaturesMap.set(sig.signerId, sig);
    }
    const signatures = Array.from(signaturesMap.values());

    return {
      id: proposalId,
      commitment: proposalId,
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

  private deltaStatusToProposalStatus(status: DeltaStatus, proposalType?: ProposalType): TransactionProposalStatus {
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
