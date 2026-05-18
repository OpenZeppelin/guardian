/**
 * Multisig class representing a created or loaded multisig account.
 *
 * This class wraps a Miden SDK Account and provides GUARDIAN integration
 * for proposal management.
 */

import { GuardianHttpClient, type DeltaObject, type ProposalSignature, type Signer, type AuthConfig, type StateObject } from '@openzeppelin/guardian-client';
import type {
  ConsumableNote,
  ExportedProposal,
  MultisigConfig,
  NoteAsset,
  Proposal,
  ProposalMetadata,
  ProposalType,
} from './types.js';
import type { ProcedureName } from './procedures.js';
import type {
  MidenClient,
  TransactionProver,
  TransactionRequest,
  WasmWebClient,
} from '@miden-sdk/miden-sdk';
import {
  Account,
  AccountId,
  AdviceMap,
  Endpoint,
  FeltArray,
  Note,
  RpcClient,
  Signature,
  TransactionSummary,
  Word,
} from '@miden-sdk/miden-sdk';
import {
  executeForSummary,
  buildUpdateSignersTransactionRequest,
  buildUpdateProcedureThresholdTransactionRequest,
  buildUpdateGuardianTransactionRequest,
  buildConsumeNotesTransactionRequest,
  buildP2idTransactionRequest,
} from './transaction.js';
import { buildConsumeNotesTransactionRequestFromNotes } from './transaction/consumeNotes.js';
import {
  CONSUME_NOTES_METADATA_VERSION_V2,
  MAX_CONSUME_NOTES_METADATA_BYTES,
} from './types/proposal.js';
import { LEGACY_CONSUME_NOTES_ENABLED } from './multisig/config.js';
import {
  ConsumeNotesMetadataOversizeError,
  LegacyConsumeNotesNoteMissingError,
  NoteBindingMismatchError,
  UnsupportedMetadataVersionError,
} from './multisig/consumeNotesErrors.js';
import { noteFromBase64, noteToBase64 } from './utils/encoding.js';
import {
  base64ToUint8Array,
  uint8ArrayToBase64,
  normalizeHexWord,
} from './utils/encoding.js';
import {
  buildSignatureAdviceEntry,
  normalizeSignerCommitment,
  signatureHexToBytes,
  tryComputeEcdsaCommitmentHex,
} from './utils/signature.js';
import { computeCommitmentFromTxSummary, accountIdToHex } from './multisig/helpers.js';
import { buildGuardianSignatureFromSigner } from './multisig/signing.js';
import { AccountInspector } from './inspector.js';
import { ProposalFactory } from './proposal/factory.js';
import { ProposalMetadataCodec } from './proposal/metadata.js';
import { ProposalSignatures } from './proposal/signatures.js';
import { getRawMidenClient, getTransactionProver } from './raw-client.js';

/**
 * Result of fetching account state from GUARDIAN.
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
 * Represents a multisig account with GUARDIAN integration.
 */
export class Multisig {
  account: Account;
  threshold: number;
  signerCommitments: string[];
  guardianCommitment: string;
  procedureThresholds: Map<ProcedureName, number>;
  guardianPublicKey?: string;

  private guardian: GuardianHttpClient;
  private readonly signer: Signer;
  private readonly midenClient: MidenClient;
  private readonly rawClientPromise: Promise<WasmWebClient>;
  private readonly transactionProver: TransactionProver | null;
  private readonly _accountId: string;
  private readonly midenRpcEndpoint?: string;
  private proposals: Map<string, Proposal> = new Map();

  constructor(
    account: Account,
    config: MultisigConfig,
    guardian: GuardianHttpClient,
    signer: Signer,
    midenClient: MidenClient,
    accountId?: string,
    midenRpcEndpoint?: string
  ) {
    this.account = account;
    this.threshold = config.threshold;
    this.signerCommitments = config.signerCommitments;
    this.guardianCommitment = config.guardianCommitment;
    this.guardianPublicKey = config.guardianPublicKey;
    this.procedureThresholds = new Map(
      (config.procedureThresholds ?? []).map((pt) => [pt.procedure, pt.threshold])
    );
    this.guardian = guardian;
    this.signer = signer;
    this.midenClient = midenClient;
    this._accountId = accountId ?? (account ? accountIdToHex(account) : '');
    this.midenRpcEndpoint = midenRpcEndpoint;
    this.rawClientPromise = getRawMidenClient(midenClient, midenRpcEndpoint);
    this.transactionProver = getTransactionProver(midenClient);
  }

  private getMidenRpcEndpoint(): string {
    if (!this.midenRpcEndpoint) {
      throw new Error('Missing Miden RPC endpoint in MultisigClient configuration');
    }
    return this.midenRpcEndpoint;
  }

  private async getRawClient(): Promise<WasmWebClient> {
    return this.rawClientPromise;
  }

  private proposalFactory(): ProposalFactory {
    return new ProposalFactory({
      accountId: this._accountId,
      signerCommitments: this.signerCommitments,
      resolveRequiredSignatures: (proposalType) => this.getEffectiveThreshold(proposalType),
    });
  }

  private async verifyGuardianEndpointCommitment(endpoint: string | undefined, expectedCommitment: string): Promise<void> {
    if (!endpoint) {
      throw new Error('Switch GUARDIAN proposal missing newGuardianEndpoint');
    }

    const endpointClient = new GuardianHttpClient(endpoint);
    const fetchedPubkey = await endpointClient.getPubkey(this.signer.scheme);
    const endpointCommitment = normalizeHexWord(fetchedPubkey.commitment);
    const normalizedExpected = normalizeHexWord(expectedCommitment);

    if (endpointCommitment !== normalizedExpected) {
      throw new Error(
        `Refusing to use GUARDIAN endpoint ${endpoint}: endpoint pubkey commitment ${endpointCommitment} does not match expected ${normalizedExpected}`
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
      case 'switch_guardian':
        return 'update_guardian';
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
   * Update the GUARDIAN client used by this Multisig instance.
   *
   * @param guardianClient - The new GUARDIAN HTTP client
   */
  setGuardianClient(guardianClient: GuardianHttpClient): void {
    this.guardian = guardianClient;
    this.guardian.setSigner(this.signer);
  }

  /**
   * Fetch the current account state from GUARDIAN.
   *
   * @returns The account state including commitment and serialized data
   */
  async fetchState(): Promise<AccountState> {
    const state: StateObject = await this.guardian.getState(this._accountId);

    return {
      accountId: state.accountId,
      commitment: state.commitment,
      stateDataBase64: state.stateJson.data,
      createdAt: state.createdAt,
      updatedAt: state.updatedAt,
    };
  }

  /**
   * Sync account state from GUARDIAN into the local Miden client store.
   *
   * If the GUARDIAN commitment differs from the local commitment (or the account
   * is missing locally), the local store is overwritten with the GUARDIAN state.
   */
  async syncState(): Promise<AccountState> {
    const state = await this.fetchState();
    const accountId = AccountId.fromHex(this._accountId);
    const webClient = await this.getRawClient();
    const localAccount = await webClient.getAccount(accountId);
    let accountForConfigRefresh: Account | null = localAccount ?? null;

    const guardianCommitment = normalizeHexWord(state.commitment);
    const localCommitment = localAccount
      ? normalizeHexWord(localAccount.to_commitment().toHex())
      : null;

    if (!localAccount || localCommitment !== guardianCommitment) {
      const accountBytes = base64ToUint8Array(state.stateDataBase64);
      const incomingAccount = Account.deserialize(accountBytes);
      await this.ensureSafeToOverwriteLocalState(incomingAccount, localAccount);
      await webClient.newAccount(incomingAccount, true);
      accountForConfigRefresh = incomingAccount;
    }

    this.refreshConfigFromAccount(accountForConfigRefresh);

    return state;
  }

  async verifyStateCommitment(): Promise<AccountStateVerificationResult> {
    const accountId = AccountId.fromHex(this._accountId);
    const webClient = await this.getRawClient();
    const localAccount = await webClient.getAccount(accountId);

    if (!localAccount) {
      throw new Error(
        `Local account state not found for account ${this._accountId}. Sync the account before verifying.`
      );
    }

    const localCommitment = normalizeHexWord(localAccount.to_commitment().toHex());
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

    const incomingCommitment = normalizeHexWord(incomingAccount.to_commitment().toHex());
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
      if (detected.guardianCommitment) {
        this.guardianCommitment = detected.guardianCommitment;
      }
      this.procedureThresholds = new Map(detected.procedureThresholds);
    } catch (error) {
      console.warn('Failed to refresh multisig config from account state', error);
    }
  }

  /**
   * Register this multisig account on the GUARDIAN server.
   *
   * The initial state must be the serialized Account bytes (base64-encoded).
   * If not provided, the account's serialize() method is used.
   *
   * @param initialStateBase64 - Optional base64-encoded serialized Account.¡
   */
  async registerOnGuardian(initialStateBase64?: string): Promise<void> {
    // Serialize the account to bytes and base64-encode
    const stateData =
      initialStateBase64 ?? uint8ArrayToBase64(this.account.serialize());

    const auth: AuthConfig =
      this.signer.scheme === 'ecdsa'
        ? {
            MidenEcdsa: {
              cosigner_commitments: this.signerCommitments,
            },
          }
        : {
            MidenFalconRpo: {
              cosigner_commitments: this.signerCommitments,
            },
          };

    const response = await this.guardian.configure({
      accountId: this._accountId,
      auth,
      initialState: { data: stateData, accountId: this._accountId },
    });

    if (!response.success) {
      throw new Error(`Failed to register on GUARDIAN: ${response.message}`);
    }
  }

  /**
   * Sync proposals from the GUARDIAN server.
   */
  async syncProposals(): Promise<Proposal[]> {
    const deltas = await this.guardian.getDeltaProposals(this._accountId);
    const factory = this.proposalFactory();

    for (const delta of deltas) {
      const proposalId = normalizeHexWord(
        computeCommitmentFromTxSummary(delta.deltaPayload.txSummary.data)
      );
      const existingProposal = this.proposals.get(proposalId);
      const proposal = factory.fromDelta(
        delta,
        proposalId,
        existingProposal?.metadata,
        existingProposal?.signatures ?? [],
      );
      await this.verifyProposalMetadataBinding(proposal);

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
    const guardianMetadata = ProposalMetadataCodec.toGuardian(metadata);

    const response = await this.guardian.pushDeltaProposal({
      accountId: this._accountId,
      nonce,
      deltaPayload: {
        txSummary: { data: txSummaryBase64 },
        signatures: [],
        metadata: guardianMetadata,
      },
    });

    const proposal = this.proposalFactory().fromDelta(response.delta, response.commitment, metadata);
    await this.verifyProposalMetadataBinding(proposal);
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
    const webClient = await this.getRawClient();
    const targetThreshold = newThreshold ?? this.threshold;
    const targetSignerCommitments = [...this.signerCommitments, newCommitment];

    const { request, salt } = await buildUpdateSignersTransactionRequest(
      webClient,
      targetThreshold,
      targetSignerCommitments,
      { signatureScheme: this.signer.scheme },
    );

    const summary = await executeForSummary(webClient, this._accountId, request);
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
    const webClient = await this.getRawClient();
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
      webClient,
      targetThreshold,
      targetSignerCommitments,
      { signatureScheme: this.signer.scheme },
    );

    const summary = await executeForSummary(webClient, this._accountId, request);
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
    const webClient = await this.getRawClient();
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
      { signatureScheme: this.signer.scheme },
    );

    const summary = await executeForSummary(webClient, this._accountId, request);
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
    const webClient = await this.getRawClient();
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
      webClient,
      targetProcedure,
      targetThreshold,
      { signatureScheme: this.signer.scheme },
    );

    const summary = await executeForSummary(webClient, this._accountId, request);
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
   * Create a "switch GUARDIAN" proposal to change the GUARDIAN provider.
   * 
   * @param newGuardianEndpoint - The new GUARDIAN server endpoint URL
   * @param newGuardianPubkey - The new GUARDIAN server's public key commitment (hex)
   * @param nonce - Optional proposal nonce (defaults to Date.now())
   */
  async createSwitchGuardianProposal(
    newGuardianEndpoint: string,
    newGuardianPubkey: string,
    nonce?: number,
  ): Promise<Proposal> {
    const webClient = await this.getRawClient();
    await this.verifyGuardianEndpointCommitment(newGuardianEndpoint, newGuardianPubkey);

    const { request, salt } = await buildUpdateGuardianTransactionRequest(
      webClient,
      newGuardianPubkey,
      { signatureScheme: this.signer.scheme },
    );

    const summary = await executeForSummary(webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'switch_guardian',
      saltHex: salt.toHex(),
      requiredSignatures: this.getEffectiveThreshold('switch_guardian'),
      newGuardianPubkey,
      newGuardianEndpoint,
      description: `Switch GUARDIAN to ${newGuardianEndpoint}`,
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
    const webClient = await this.getRawClient();
    if (noteIds.length === 0) {
      throw new Error('At least one note ID is required');
    }

    // Fetch notes locally (proposer has them per FR-012); embed for v2 verification.
    const rawClient = await getRawMidenClient(webClient);
    const fetchedNotes: Note[] = [];
    for (const noteIdHex of noteIds) {
      const inputNoteRecord = await rawClient.getInputNote(noteIdHex);
      if (!inputNoteRecord) {
        throw new LegacyConsumeNotesNoteMissingError(noteIdHex);
      }
      fetchedNotes.push(inputNoteRecord.toNote());
    }
    const embeddedNotes = fetchedNotes.map((n) => noteToBase64(n));

    const { request, salt } = buildConsumeNotesTransactionRequestFromNotes(fetchedNotes);

    const summary = await executeForSummary(webClient, this._accountId, request);
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const proposalNonce = nonce ?? Date.now();

    const metadata: ProposalMetadata = {
      proposalType: 'consume_notes',
      noteIds,
      metadataVersion: CONSUME_NOTES_METADATA_VERSION_V2,
      notes: embeddedNotes,
      saltHex: salt.toHex(),
      requiredSignatures: this.getEffectiveThreshold('consume_notes'),
      description: `Consume ${noteIds.length} note(s)`,
    };

    // FR-011: enforce metadata size cap on the wire-encoded form (what GUARDIAN
    // actually persists), matching the Rust side which measures
    // `ProposalMetadataPayload`. Sizing the local in-memory `metadata` would
    // miss codec divergence.
    const encoded = ProposalMetadataCodec.toGuardian(metadata);
    const metadataSize = new TextEncoder().encode(JSON.stringify(encoded)).length;
    if (metadataSize > MAX_CONSUME_NOTES_METADATA_BYTES) {
      throw new ConsumeNotesMetadataOversizeError(MAX_CONSUME_NOTES_METADATA_BYTES, metadataSize);
    }

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
    const webClient = await this.getRawClient();
    if (amount <= 0n) {
      throw new Error('Amount must be greater than 0');
    }

    const { request, salt } = buildP2idTransactionRequest(
      this._accountId,
      recipientId,
      faucetId,
      amount,
    );

    const summary = await executeForSummary(webClient, this._accountId, request);
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
    const webClient = await this.getRawClient();

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
    const normalizedProposalId = normalizeHexWord(proposalId);
    const existingProposal = await this.getProposalForSigning(proposalId, normalizedProposalId);
    if (!existingProposal) {
      throw new Error(`Proposal not found: ${proposalId}`);
    }
    this.proposalFactory().assertAccountId(existingProposal.accountId);
    const factory = this.proposalFactory();
    const proposal = existingProposal;

    const commitmentToSign = await this.verifyProposalMetadataBinding(proposal);
    const signature: ProposalSignature = await buildGuardianSignatureFromSigner(
      this.signer,
      commitmentToSign,
    );

    const signedDelta = await this.guardian.signDeltaProposal({
      accountId: this._accountId,
      commitment: normalizedProposalId,
      signature,
    });

    const signedProposal = factory.fromDelta(
      signedDelta,
      normalizedProposalId,
      proposal.metadata,
      proposal.signatures,
    );
    await this.verifyProposalMetadataBinding(signedProposal);

    this.proposals.set(signedProposal.id, signedProposal);

    return signedProposal;
  }

  private async getProposalForSigning(
    proposalId: string,
    normalizedProposalId: string,
  ): Promise<Proposal | undefined> {
    const cachedProposal = this.proposals.get(proposalId);
    if (cachedProposal) {
      return cachedProposal;
    }

    await this.syncProposals();
    return this.proposals.get(proposalId) ?? this.proposals.get(normalizedProposalId);
  }

  async createTransactionProposalRequest(proposalId: string): Promise<TransactionRequest> {
    const { finalRequest } = await this.prepareProposalExecution(proposalId);
    return finalRequest;
  }

  /**
   * Execute a proposal that has enough signatures.
   *
   * @param proposalId - The proposal commitment/ID
   */
  async executeProposal(proposalId: string): Promise<void> {
    const { metadata, finalRequest, proposal } = await this.prepareProposalExecution(proposalId);

    const accountId = AccountId.fromHex(this._accountId);
    await this.midenClient.transactions.submit(accountId, finalRequest);

    if (metadata.proposalType === 'switch_guardian') {
      if (!metadata.newGuardianEndpoint || !metadata.newGuardianPubkey) {
        throw new Error('Switch GUARDIAN proposal metadata is incomplete after execution');
      }

      try {
        const webClient = await this.getRawClient();
        await webClient.syncState();

        const updatedAccount = await webClient.getAccount(accountId);
        if (!updatedAccount) {
          throw new Error(
            `Updated account ${this._accountId} is missing from local client`
          );
        }

        const updatedStateBase64 = uint8ArrayToBase64(updatedAccount.serialize());
        const nextGuardian = new GuardianHttpClient(metadata.newGuardianEndpoint);
        this.setGuardianClient(nextGuardian);
        this.guardianPublicKey = metadata.newGuardianPubkey;

        await this.registerOnGuardian(updatedStateBase64);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        throw new Error(
          `Transaction executed successfully but failed to register on new GUARDIAN: ${message}`
        );
      }
    }

    proposal.status = 'finalized';
  }

  private getLocalProposal(proposalId: string): Proposal | undefined {
    const normalizedProposalId = normalizeHexWord(proposalId);
    return this.proposals.get(proposalId) ?? this.proposals.get(normalizedProposalId);
  }

  private async prepareProposalExecution(
    proposalId: string,
  ): Promise<{ finalRequest: TransactionRequest; metadata: ProposalMetadata; proposal: Proposal }> {
    const proposal = this.getLocalProposal(proposalId);
    if (!proposal) {
      throw new Error(`Proposal not found: ${proposalId}`);
    }

    this.proposalFactory().assertAccountId(proposal.accountId);
    await this.verifyProposalMetadataBinding(proposal);

    const metadata = proposal.metadata;
    const effectiveThreshold = this.getEffectiveThreshold(metadata.proposalType);
    const signatureContext = `Invalid proposal signatures for ${proposalId}`;
    const signaturesForExecution = new ProposalSignatures(
      proposal.signatures,
      this.signerCommitments,
      signatureContext,
    ).entries();

    if (signaturesForExecution.length < effectiveThreshold) {
      throw new Error('Proposal is not ready for execution. Still pending signatures.');
    }

    const isSwitchGuardian = metadata.proposalType === 'switch_guardian';
    const normalizedProposalId = normalizeHexWord(proposal.id);

    let txSummaryBase64: string;
    let delta: DeltaObject | undefined;

    if (isSwitchGuardian) {
      txSummaryBase64 = proposal.txSummary;
    } else {
      delta = await this.guardian.getDeltaProposal(this._accountId, normalizedProposalId);
      txSummaryBase64 = delta.deltaPayload.txSummary.data;
    }

    const txSummaryBytes = base64ToUint8Array(txSummaryBase64);
    const txSummary = TransactionSummary.deserialize(txSummaryBytes);
    const saltHex = txSummary.salt().toHex();
    const txCommitmentHex = txSummary.toCommitment().toHex();
    const normalizedTxCommitmentHex = normalizeHexWord(txCommitmentHex);
    const normalizedSignerCommitments = new Set(
      this.signerCommitments.map((commitment) => normalizeHexWord(commitment)),
    );
    const adviceMap = new AdviceMap();
    const adviceMapKeys = new Set<string>();
    const createTxCommitmentWord = (): Word => Word.fromHex(normalizedTxCommitmentHex);

    for (const cosignerSig of signaturesForExecution) {
      let signerCommitmentHex = normalizeHexWord(cosignerSig.signerId);
      const ecdsaPublicKey =
        cosignerSig.signature.scheme === 'ecdsa'
          ? cosignerSig.signature.publicKey
          : undefined;

      if (cosignerSig.signature.scheme === 'ecdsa') {
        if (!ecdsaPublicKey) {
          throw new Error(
            `ECDSA proposal signature for ${signerCommitmentHex} is missing publicKey`,
          );
        }

        const derivedCommitment = tryComputeEcdsaCommitmentHex(ecdsaPublicKey);
        if (derivedCommitment && derivedCommitment !== signerCommitmentHex) {
          if (!normalizedSignerCommitments.has(derivedCommitment)) {
            throw new Error(
              `ECDSA public key commitment mismatch: derived commitment ${derivedCommitment} is not in signerCommitments.`,
            );
          }
          signerCommitmentHex = derivedCommitment;
        }
      }

      const signerCommitment = Word.fromHex(signerCommitmentHex);
      const sigBytes = signatureHexToBytes(
        cosignerSig.signature.signature,
        cosignerSig.signature.scheme,
      );
      const signature = Signature.deserialize(sigBytes);
      const { key, values } = buildSignatureAdviceEntry(
        signerCommitment,
        createTxCommitmentWord(),
        signature,
        ecdsaPublicKey,
        cosignerSig.signature.scheme === 'ecdsa'
          ? cosignerSig.signature.signature
          : undefined,
      );
      const keyHex = normalizeHexWord(key.toHex());
      if (adviceMapKeys.has(keyHex)) {
        throw new Error(`Duplicate advice-map key detected for proposal ${proposalId}`);
      }
      adviceMapKeys.add(keyHex);
      adviceMap.insert(key, new FeltArray(values));
    }

    if (!isSwitchGuardian && delta) {
      const executionDelta = {
        ...delta,
        deltaPayload: delta.deltaPayload.txSummary,
      };

      const pushResult = await this.guardian.pushDelta(executionDelta);
      const ackSigHex = pushResult.ackSig;
      if (!ackSigHex) {
        throw new Error('GUARDIAN did not return acknowledgment signature');
      }

      const guardianCommitment = Word.fromHex(normalizeHexWord(this.guardianCommitment));
      const ackScheme = (pushResult.ackScheme as 'ecdsa' | 'falcon') || this.signer.scheme;
      const ackPubkey = pushResult.ackPubkey || this.guardianPublicKey;
      if (ackScheme === 'ecdsa' && !ackPubkey) {
        throw new Error('GUARDIAN acknowledgment is missing ECDSA public key');
      }
      if (ackScheme === 'ecdsa' && ackPubkey) {
        const derivedCommitment = tryComputeEcdsaCommitmentHex(ackPubkey);
        if (derivedCommitment && derivedCommitment !== normalizeHexWord(this.guardianCommitment)) {
          throw new Error('GUARDIAN public key commitment mismatch');
        }
      }
      const ackSigBytes = signatureHexToBytes(ackSigHex, ackScheme);
      const ackSignature = Signature.deserialize(ackSigBytes);
      const { key: ackKey, values: ackValues } = buildSignatureAdviceEntry(
        guardianCommitment,
        createTxCommitmentWord(),
        ackSignature,
        ackScheme === 'ecdsa' ? ackPubkey : undefined,
        ackScheme === 'ecdsa' ? ackSigHex : undefined,
      );
      const ackKeyHex = normalizeHexWord(ackKey.toHex());
      if (adviceMapKeys.has(ackKeyHex)) {
        throw new Error(`Duplicate advice-map key detected for GUARDIAN acknowledgment in proposal ${proposalId}`);
      }
      adviceMapKeys.add(ackKeyHex);
      adviceMap.insert(ackKey, new FeltArray(ackValues));
    }

    if (metadata.proposalType === 'switch_guardian') {
      await this.verifyGuardianEndpointCommitment(metadata.newGuardianEndpoint, metadata.newGuardianPubkey);
    }

    const executionSalt = Word.fromHex(normalizeHexWord(saltHex));
    const finalRequest = await this.buildTransactionRequestFromMetadata(
      metadata,
      executionSalt,
      adviceMap,
    );

    return { finalRequest, metadata, proposal };
  }

  /**
   * Export a proposal for offline signing
   */
  async exportProposal(proposalId: string): Promise<ExportedProposal> {
    const delta = await this.guardian.getDeltaProposal(this._accountId, proposalId);
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
            scheme: s.signature.scheme,
            publicKey: s.signature.scheme === 'ecdsa' ? s.signature.publicKey : undefined,
            timestamp: s.timestamp,
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
        scheme: s.signature.scheme,
        publicKey: s.signature.scheme === 'ecdsa' ? s.signature.publicKey : undefined,
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
  async importProposal(json: string): Promise<Proposal> {
    const exported: ExportedProposal = JSON.parse(json);
    if (!exported.accountId || !exported.txSummaryBase64 || !exported.commitment || !exported.metadata) {
      throw new Error('Invalid proposal JSON: missing required fields');
    }

    const proposal = this.proposalFactory().fromExported(exported);

    await this.verifyProposalMetadataBinding(proposal);
    this.proposals.set(proposal.id, proposal);

    return proposal;
  }

  /**
   * Sign an imported proposal and return updated JSON for sharing..
   *
   * @param proposalId - The proposal commitment/ID
   * @returns Updated JSON string with the new signature included
   */
  async signProposalOffline(proposalId: string): Promise<string> {
    const normalizedProposalId = normalizeHexWord(proposalId);
    const proposal = this.proposals.get(proposalId) ?? this.proposals.get(normalizedProposalId);
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

    const commitmentToSign = await this.verifyProposalMetadataBinding(proposal);

    // Sign the commitment
    const signature = await buildGuardianSignatureFromSigner(this.signer, commitmentToSign);

    // Add signature to local proposal
    const signatures = [
      ...existingSignatures.entries(),
      {
        signerId: signerCommitment,
        signature,
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
    return this.exportProposalToJson(proposal.id);
  }

  private ensureProposalCommitmentMatchesSummary(proposal: Proposal): string {
    const proposalId = normalizeHexWord(proposal.id);
    const txSummaryCommitment = normalizeHexWord(
      computeCommitmentFromTxSummary(proposal.txSummary)
    );
    if (proposalId !== txSummaryCommitment) {
      throw new Error(
        `Invalid proposal: id ${proposal.id} does not match tx_summary commitment ${txSummaryCommitment}`
      );
    }
    return txSummaryCommitment;
  }

  private async verifyProposalMetadataBinding(proposal: Proposal): Promise<string> {
    const txSummaryCommitment = this.ensureProposalCommitmentMatchesSummary(proposal);
    if (proposal.metadata.proposalType === 'unknown') {
      throw new Error(`Cannot verify proposal metadata for unknown proposal type: ${proposal.id}`);
    }

    const summary = TransactionSummary.deserialize(base64ToUint8Array(proposal.txSummary));
    const salt = proposal.metadata.saltHex
      ? Word.fromHex(normalizeHexWord(proposal.metadata.saltHex))
      : summary.salt();

    const request = await this.buildTransactionRequestFromMetadata(proposal.metadata, salt);
    const webClient = await this.getRawClient();
    const reconstructed = await executeForSummary(webClient, this._accountId, request);
    const reconstructedCommitment = normalizeHexWord(reconstructed.toCommitment().toHex());

    if (reconstructedCommitment !== txSummaryCommitment) {
      throw new Error(`Invalid proposal: metadata does not match tx_summary for ${proposal.id}`);
    }

    return txSummaryCommitment;
  }

  private async buildTransactionRequestFromMetadata(
    metadata: ProposalMetadata,
    salt: Word,
    signatureAdviceMap?: AdviceMap,
  ): Promise<TransactionRequest> {
    const webClient = await this.getRawClient();

    switch (metadata.proposalType) {
      case 'add_signer':
      case 'remove_signer':
      case 'change_threshold': {
        const { request } = await buildUpdateSignersTransactionRequest(
          webClient,
          metadata.targetThreshold,
          metadata.targetSignerCommitments,
          { salt, signatureAdviceMap, signatureScheme: this.signer.scheme }
        );
        return request;
      }
      case 'switch_guardian': {
        const { request } = await buildUpdateGuardianTransactionRequest(
          webClient,
          metadata.newGuardianPubkey,
          { salt, signatureAdviceMap, signatureScheme: this.signer.scheme }
        );
        return request;
      }
      case 'update_procedure_threshold': {
        const { request } = await buildUpdateProcedureThresholdTransactionRequest(
          webClient,
          metadata.targetProcedure,
          metadata.targetThreshold,
          { salt, signatureAdviceMap, signatureScheme: this.signer.scheme }
        );
        return request;
      }
      case 'consume_notes': {
        // v1/v2 dispatch for issue #229 / FR-009.
        const version = metadata.metadataVersion;
        if (version === CONSUME_NOTES_METADATA_VERSION_V2) {
          const embedded = metadata.notes ?? [];
          if (embedded.length !== metadata.noteIds.length) {
            throw new NoteBindingMismatchError(
              `consume_notes v2: notes.length=${embedded.length} does not match noteIds.length=${metadata.noteIds.length}`,
            );
          }
          const decoded: Note[] = [];
          for (let i = 0; i < embedded.length; i++) {
            const note = noteFromBase64(embedded[i], Note);
            // Normalize both sides; matches the file's other hex comparisons.
            const embeddedId = normalizeHexWord(note.id().toString());
            const declaredId = normalizeHexWord(metadata.noteIds[i]);
            if (embeddedId !== declaredId) {
              throw new NoteBindingMismatchError(
                `consume_notes v2: notes[${i}] id ${embeddedId} != noteIds[${i}] ${declaredId}`,
              );
            }
            decoded.push(note);
          }
          const { request } = buildConsumeNotesTransactionRequestFromNotes(decoded, {
            salt,
            signatureAdviceMap,
          });
          return request;
        }
        if (version === undefined || version === 1) {
          if (!LEGACY_CONSUME_NOTES_ENABLED) {
            // Preserve explicit `1` vs absent so the error tells the
            // operator which legacy shape was rejected.
            throw new UnsupportedMetadataVersionError(version);
          }
          const { request } = await buildConsumeNotesTransactionRequest(
            webClient,
            metadata.noteIds,
            { salt, signatureAdviceMap },
          );
          return request;
        }
        throw new UnsupportedMetadataVersionError(version);
      }
      case 'p2id': {
        const { request } = buildP2idTransactionRequest(
          this._accountId,
          metadata.recipientId,
          metadata.faucetId,
          BigInt(metadata.amount),
          { salt, signatureAdviceMap }
        );
        return request;
      }
      case 'unknown':
        throw new Error('Unsupported proposal type: unknown');
    }
  }

}
