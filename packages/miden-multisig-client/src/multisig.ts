/**
 * Multisig class representing a created or loaded multisig account.
 *
 * This class wraps a Miden SDK Account and provides PSM integration
 * for proposal management.
 */

import { PsmHttpClient, type DeltaObject, type FalconSignature, type Signer, type AuthConfig, type StateObject } from '@openzeppelin/psm-client';
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
import type { WebClient, TransactionRequest } from '@miden-sdk/miden-sdk';
import {
  Account,
  AccountId,
  AdviceMap,
  Endpoint,
  FeltArray,
  RpcClient,
  Signature,
  TransactionSummary,
  Word,
} from '@miden-sdk/miden-sdk';
import {
  executeForSummary,
  buildUpdateSignersTransactionRequest,
  buildUpdateProcedureThresholdTransactionRequest,
  buildUpdatePsmTransactionRequest,
  buildConsumeNotesTransactionRequest,
  buildP2idTransactionRequest,
} from './transaction.js';
import {
  base64ToUint8Array,
  uint8ArrayToBase64,
  normalizeHexWord,
} from './utils/encoding.js';
import {
  buildSignatureAdviceEntry,
  normalizeSignerCommitment,
  signatureHexToBytes,
} from './utils/signature.js';
import { computeCommitmentFromTxSummary, accountIdToHex } from './multisig/helpers.js';
import { AccountInspector } from './inspector.js';
import { ProposalFactory } from './proposal/factory.js';
import { ProposalMetadataCodec } from './proposal/metadata.js';
import { ProposalSignatures } from './proposal/signatures.js';

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

export interface AccountStateVerificationResult {
  accountId: string;
  localCommitment: string;
  onChainCommitment: string;
}

/**
 * Represents a multisig account with PSM integration.
 */
export class Multisig {
  account: Account;
  threshold: number;
  signerCommitments: string[];
  psmCommitment: string;
  procedureThresholds: Map<ProcedureName, number>;

  private psm: PsmHttpClient;
  private readonly signer: Signer;
  private readonly webClient: WebClient;
  private readonly _accountId: string;
  private readonly midenRpcEndpoint?: string;
  private proposals: Map<string, Proposal> = new Map();

  constructor(
    account: Account,
    config: MultisigConfig,
    psm: PsmHttpClient,
    signer: Signer,
    webClient: WebClient,
    accountId?: string,
    midenRpcEndpoint?: string
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
    this.midenRpcEndpoint = midenRpcEndpoint;
  }

  private getMidenRpcEndpoint(): string {
    if (!this.midenRpcEndpoint) {
      throw new Error('Missing Miden RPC endpoint in MultisigClient configuration');
    }
    return this.midenRpcEndpoint;
  }

  private proposalFactory(): ProposalFactory {
    return new ProposalFactory({
      accountId: this._accountId,
      signerCommitments: this.signerCommitments,
      resolveRequiredSignatures: (proposalType) => this.getEffectiveThreshold(proposalType),
    });
  }

  private async verifyPsmEndpointCommitment(endpoint: string | undefined, expectedCommitment: string): Promise<void> {
    if (!endpoint) {
      throw new Error('Switch PSM proposal missing newPsmEndpoint');
    }

    const endpointClient = new PsmHttpClient(endpoint);
    const endpointCommitment = normalizeHexWord(await endpointClient.getPubkey());
    const normalizedExpected = normalizeHexWord(expectedCommitment);

    if (endpointCommitment !== normalizedExpected) {
      throw new Error(
        `Refusing to use PSM endpoint ${endpoint}: endpoint pubkey commitment ${endpointCommitment} does not match expected ${normalizedExpected}`
      );
    }
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
      case 'update_procedure_threshold':
        return 'update_procedure_threshold';
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
    let accountForConfigRefresh: Account | null = localAccount ?? null;

    const psmCommitment = normalizeHexWord(state.commitment);
    const localCommitment = localAccount
      ? normalizeHexWord(localAccount.commitment().toHex())
      : null;

    if (!localAccount || localCommitment !== psmCommitment) {
      const accountBytes = base64ToUint8Array(state.stateDataBase64);
      const incomingAccount = Account.deserialize(accountBytes);
      await this.ensureSafeToOverwriteLocalState(incomingAccount, localAccount);
      await this.webClient.newAccount(incomingAccount, true);
      accountForConfigRefresh = incomingAccount;
    }

    this.refreshConfigFromAccount(accountForConfigRefresh);

    return state;
  }

  async verifyStateCommitment(): Promise<AccountStateVerificationResult> {
    const accountId = AccountId.fromHex(this._accountId);
    const localAccount = await this.webClient.getAccount(accountId);

    if (!localAccount) {
      throw new Error(
        `Local account state not found for account ${this._accountId}. Sync the account before verifying.`
      );
    }

    const localCommitment = normalizeHexWord(localAccount.commitment().toHex());
    const onChainCommitment = await this.getOnChainCommitment(accountId);

    if (!onChainCommitment) {
      throw new Error(`On-chain account details not found for account ${this._accountId}`);
    }

    if (localCommitment !== onChainCommitment) {
      throw new Error(
        `Local account commitment does not match on-chain commitment for account ${this._accountId}`
      );
    }

    return {
      accountId: this._accountId,
      localCommitment,
      onChainCommitment,
    };
  }

  private async ensureSafeToOverwriteLocalState(
    incomingAccount: Account,
    localAccount?: Account,
  ): Promise<void> {
    if (localAccount) {
      const localNonce = localAccount.nonce().asInt();
      const incomingNonce = incomingAccount.nonce().asInt();

      if (incomingNonce <= localNonce) {
        throw new Error(
          `Refusing to overwrite local state: incoming nonce ${incomingNonce.toString()} is not greater than local nonce ${localNonce.toString()} for account ${this._accountId}`
        );
      }
    }

    const accountId = AccountId.fromHex(this._accountId);
    const onChainCommitment = await this.getOnChainCommitment(accountId);
    if (!onChainCommitment) {
      return;
    }

    const incomingCommitment = normalizeHexWord(incomingAccount.commitment().toHex());
    if (incomingCommitment !== onChainCommitment) {
      throw new Error(
        `Refusing to overwrite local state: incoming commitment does not match on-chain commitment for account ${this._accountId}`
      );
    }
  }

  private async getOnChainCommitment(accountId: AccountId): Promise<string | null> {
    const rpcClient = new RpcClient(new Endpoint(this.getMidenRpcEndpoint()));

    try {
      const accountDetails = await rpcClient.getAccountDetails(accountId);
      // If the account is not found or its commitment is zero, means that the account is not deployed yet
      if (!accountDetails) {
        return null;
      }
      const commitment = normalizeHexWord(accountDetails.commitment().toHex());
      const zeroCommitment = `0x${'0'.repeat(64)}`;
      if (commitment === zeroCommitment) {
        return null;
      }
      return commitment;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (
        message.includes('null pointer passed to rust') ||
        message.includes('No account header record found for given ID') ||
        message.toLowerCase().includes('not found')
      ) {
        return null;
      }
      throw error;
    }
  }

  private refreshConfigFromAccount(account: Account | null): void {
    if (!account) {
      return;
    }

    try {
      const detected = AccountInspector.fromAccount(account);
      this.account = account;
      this.threshold = detected.threshold;
      this.signerCommitments = detected.signerCommitments;
      if (detected.psmCommitment) {
        this.psmCommitment = detected.psmCommitment;
      }
      this.procedureThresholds = new Map(detected.procedureThresholds);
    } catch (error) {
      console.warn('Failed to refresh multisig config from account state', error);
    }
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
    // Serialize the account to bytes and base64-encode
    const stateData =
      initialStateBase64 ?? uint8ArrayToBase64(this.account.serialize());

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
    const factory = this.proposalFactory();

    for (const delta of deltas) {
      const proposalId = computeCommitmentFromTxSummary(delta.deltaPayload.txSummary.data);
      const existingProposal = this.proposals.get(proposalId);

      const proposal = factory.fromDelta(
        delta,
        proposalId,
        existingProposal?.metadata,
        existingProposal?.signatures ?? [],
      );

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
    const psmMetadata = ProposalMetadataCodec.toPsm(metadata);

    const response = await this.psm.pushDeltaProposal({
      accountId: this._accountId,
      nonce,
      deltaPayload: {
        txSummary: { data: txSummaryBase64 },
        signatures: [],
        metadata: psmMetadata,
      },
    });

    const proposal = this.proposalFactory().fromDelta(response.delta, response.commitment, metadata);
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
      requiredSignatures: this.getEffectiveThreshold('add_signer'),
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
    const targetSignerCommitments = this.signerCommitments.filter(
      (c) => c.toLowerCase() !== normalizedRemove
    );
    if (targetSignerCommitments.length === this.signerCommitments.length) {
      throw new Error(`Signer ${signerToRemove} is not in the current signer list`);
    }

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
      requiredSignatures: this.getEffectiveThreshold('remove_signer'),
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
      requiredSignatures: this.getEffectiveThreshold('change_threshold'),
      description: `Change threshold from ${this.threshold} to ${newThreshold}`,
    };

    return this.createProposal(proposalNonce, summaryBase64, metadata);
  }

  async createUpdateProcedureThresholdProposal(
    targetProcedure: ProcedureName,
    targetThreshold: number,
    nonce?: number,
  ): Promise<Proposal> {
    if (targetThreshold < 0 || targetThreshold > this.signerCommitments.length) {
      throw new Error(
        `Invalid threshold ${targetThreshold}. Must be between 0 and ${this.signerCommitments.length}`
      );
    }

    const currentOverride = this.procedureThresholds.get(targetProcedure);
    if (targetThreshold === 0 && currentOverride === undefined) {
      throw new Error(`Procedure ${targetProcedure} does not have an override to clear`);
    }

    if (currentOverride !== undefined && currentOverride === targetThreshold) {
      throw new Error(
        `Procedure ${targetProcedure} already has threshold override ${targetThreshold}`
      );
    }

    const { request, salt } = await buildUpdateProcedureThresholdTransactionRequest(
      this.webClient,
      targetProcedure,
      targetThreshold,
    );

    const summary = await executeForSummary(this.webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = nonce ?? Date.now();
    const action = targetThreshold === 0
      ? `Clear threshold override for ${targetProcedure}`
      : `Set ${targetProcedure} threshold override to ${targetThreshold}`;

    const metadata: ProposalMetadata = {
      proposalType: 'update_procedure_threshold',
      targetProcedure,
      targetThreshold,
      saltHex: salt.toHex(),
      requiredSignatures: this.getEffectiveThreshold('update_procedure_threshold'),
      description: action,
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
    await this.verifyPsmEndpointCommitment(newPsmEndpoint, newPsmPubkey);

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
      requiredSignatures: this.getEffectiveThreshold('switch_psm'),
      newPsmPubkey,
      newPsmEndpoint,
      description: `Switch PSM to ${newPsmEndpoint}`,
    };

    const proposalId = computeCommitmentFromTxSummary(summaryBase64);
    const proposal: Proposal = {
      id: proposalId,
      accountId: this._accountId,
      nonce: proposalNonce,
      status: 'pending',
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

    const { request, salt } = await buildConsumeNotesTransactionRequest(this.webClient, noteIds);

    const summary = await executeForSummary(this.webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'consume_notes',
      noteIds,
      saltHex: salt.toHex(),
      requiredSignatures: this.getEffectiveThreshold('consume_notes'),
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
      requiredSignatures: this.getEffectiveThreshold('p2id'),
      recipientId,
      faucetId,
      amount: amount.toString(),
      description: `Send ${amount} of asset ${faucetId.slice(0, 10)}... to ${recipientId.slice(0, 10)}...`,
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
               c.consumptionStatus().consumableAfterBlock() === undefined
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
    const proposalDelta = await this.psm.getDeltaProposal(this._accountId, proposalId);
    const factory = this.proposalFactory();
    const proposal = factory.fromDelta(
      proposalDelta,
      proposalId,
      existingProposal?.metadata,
      existingProposal?.signatures ?? [],
    );

    const signatureHex = this.signer.signCommitment(proposalId);

    const signature: FalconSignature = {
      scheme: 'falcon',
      signature: signatureHex,
    };

    const signedDelta = await this.psm.signDeltaProposal({
      accountId: this._accountId,
      commitment: proposalId,
      signature,
    });

    const signedProposal = factory.fromDelta(
      signedDelta,
      proposalId,
      proposal.metadata,
      proposal.signatures,
    );

    this.proposals.set(signedProposal.id, signedProposal);

    return signedProposal;
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

    const signatureContext = `Invalid proposal signatures for ${proposalId}`;
    const signaturesForExecution = new ProposalSignatures(
      proposal.signatures,
      this.signerCommitments,
      signatureContext,
    ).entries();

    if (signaturesForExecution.length < effectiveThreshold) {
      throw new Error('Proposal is not ready for execution. Still pending signatures.');
    }

    const isSwitchPsm = proposalType === 'switch_psm';

    let txSummaryBase64: string;
    let delta: DeltaObject | undefined;

    if (isSwitchPsm) {
      txSummaryBase64 = proposal.txSummary;
    } else {
      delta = await this.psm.getDeltaProposal(this._accountId, proposalId);
      txSummaryBase64 = delta.deltaPayload.txSummary.data;
    }

    const txSummaryBytes = base64ToUint8Array(txSummaryBase64);
    const txSummary = TransactionSummary.deserialize(txSummaryBytes);
    const saltHex = txSummary.salt().toHex();
    const txCommitmentHex = txSummary.toCommitment().toHex();

    const adviceMap = new AdviceMap();
    const adviceMapKeys = new Set<string>();

    for (const cosignerSig of signaturesForExecution) {
      const signerCommitment = Word.fromHex(cosignerSig.signerId);
      const sigBytes = signatureHexToBytes(cosignerSig.signature.signature);
      const signature = Signature.deserialize(sigBytes);
      const txCommitment = Word.fromHex(normalizeHexWord(txCommitmentHex));
      const { key, values } = buildSignatureAdviceEntry(
        signerCommitment,
        txCommitment,
        signature
      );
      const keyHex = normalizeHexWord(key.toHex());
      if (adviceMapKeys.has(keyHex)) {
        throw new Error(`Duplicate advice-map key detected for proposal ${proposalId}`);
      }
      adviceMapKeys.add(keyHex);
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
      const ackKeyHex = normalizeHexWord(ackKey.toHex());
      if (adviceMapKeys.has(ackKeyHex)) {
        throw new Error(`Duplicate advice-map key detected for PSM acknowledgment in proposal ${proposalId}`);
      }
      adviceMapKeys.add(ackKeyHex);
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
        const { request } = await buildConsumeNotesTransactionRequest(
          this.webClient,
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

        await this.verifyPsmEndpointCommitment(metadata.newPsmEndpoint, metadata.newPsmPubkey);

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
      case 'update_procedure_threshold': {
        const { request } = await buildUpdateProcedureThresholdTransactionRequest(
          this.webClient,
          metadata.targetProcedure,
          metadata.targetThreshold,
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

    if (metadata.proposalType === 'switch_psm') {
      if (!metadata.newPsmEndpoint || !metadata.newPsmPubkey) {
        throw new Error('Switch PSM proposal metadata is incomplete after execution');
      }

      try {
        await this.webClient.syncState();

        const updatedAccount = await this.webClient.getAccount(accountId);
        if (!updatedAccount) {
          throw new Error(
            `Updated account ${this._accountId} is missing from local client`
          );
        }

        const updatedStateBase64 = uint8ArrayToBase64(updatedAccount.serialize());
        const nextPsm = new PsmHttpClient(metadata.newPsmEndpoint);
        this.setPsmClient(nextPsm);

        await this.registerOnPsm(updatedStateBase64);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        throw new Error(
          `Transaction executed successfully but failed to register on new PSM: ${message}`
        );
      }
    }

    proposal.status = 'finalized';
  }

  /**
   * Export a proposal for offline signing
   */
  async exportProposal(proposalId: string): Promise<ExportedProposal> {
    const delta = await this.psm.getDeltaProposal(this._accountId, proposalId);
    const existingProposal = this.proposals.get(proposalId);
    const proposal = this.proposalFactory().fromDelta(
      delta,
      proposalId,
      existingProposal?.metadata,
      existingProposal?.signatures ?? [],
    );

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
      metadata: proposal.metadata,
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
    if (!exported.accountId || !exported.txSummaryBase64 || !exported.commitment || !exported.metadata) {
      throw new Error('Invalid proposal JSON: missing required fields');
    }

    const proposal = this.proposalFactory().fromExported(exported);

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
    this.proposalFactory().assertAccountId(proposal.accountId);

    const localSignatureContext = `Invalid local proposal signatures for ${proposalId}`;
    const existingSignatures = new ProposalSignatures(
      proposal.signatures,
      this.signerCommitments,
      localSignatureContext,
    );
    let signerCommitment: string;
    try {
      signerCommitment = normalizeSignerCommitment(this.signer.commitment);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      throw new Error(`Invalid local signer commitment: ${message}`);
    }

    // Check if already signed
    const alreadySigned = existingSignatures.hasSigner(signerCommitment);
    if (alreadySigned) {
      throw new Error('You have already signed this proposal');
    }

    // Sign the commitment
    const signatureHex = this.signer.signCommitment(proposalId);

    // Add signature to local proposal
    const signatures = [
      ...existingSignatures.entries(),
      {
        signerId: signerCommitment,
        signature: { scheme: 'falcon' as const, signature: signatureHex },
        timestamp: new Date().toISOString(),
      },
    ];
    const canonicalizedSignatures = new ProposalSignatures(
      signatures,
      this.signerCommitments,
      localSignatureContext,
    ).entries();
    proposal.signatures = canonicalizedSignatures;

    // Update status
    const proposalType = proposal.metadata?.proposalType;
    const signaturesRequired = proposalType
      ? this.getEffectiveThreshold(proposalType)
      : this.threshold;
    proposal.status = proposal.signatures.length >= signaturesRequired ? 'ready' : 'pending';

    // Return updated JSON
    return this.exportProposalToJson(proposalId);
  }

}
