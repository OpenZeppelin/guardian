/**
 * Multisig class representing a created or loaded multisig account.
 *
 * This class wraps a Miden SDK Account and provides GUARDIAN integration
 * for proposal management.
 */

import { GuardianHttpClient, type AbandonCandidateResponse, type AbandonStatus, type DeltaObject, type HistoryOptions, type HistoryPage, type ProposalSignature, type Signer, type AuthConfig, type StateObject } from '@openzeppelin/guardian-client';
import type {
  ConsumableNote,
  ExportedProposal,
  MultisigConfig,
  NoteAsset,
  Proposal,
  ProposalMetadata,
  ProposalSignatureEntry,
  ProposalType,
} from './types.js';
import type { ProcedureName } from './procedures.js';
import type {
  MidenClient,
  WasmWebClient,
} from '@miden-sdk/miden-sdk';
import {
  Account,
  AccountId,
  AdviceMap,
  Endpoint,
  FeltArray,
  Note,
  NoteExportFormat,
  NoteFile,
  NoteType,
  RpcClient,
  Signature,
  TransactionRequest,
  TransactionSummary,
  Word,
  type ChainAnchor,
} from '@miden-sdk/miden-sdk';
import {
  chainAnchorFromBase64,
  chainAnchorToBase64,
  executeForSummary,
  executeForSummaryAt,
  summarySalt,
  buildUpdateSignersTransactionRequest,
  buildUpdateProcedureThresholdTransactionRequest,
  buildUpdateGuardianTransactionRequest,
  buildConsumeNotesTransactionRequest,
  buildP2idNoteFromMetadata,
  buildP2idTransactionRequest,
  parseP2idNoteType,
  p2idNoteTypeToMetadata,
  type P2ideHeightOptions,
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
  assertEcdsaSignatureRecoverable,
  buildSignatureAdviceEntry,
  normalizeSignerCommitment,
  signatureHexToBytes,
  tryComputeEcdsaCommitmentHex,
} from './utils/signature.js';
import { computeCommitmentFromTxSummary, accountIdToHex } from './multisig/helpers.js';
import { buildGuardianSignatureFromSigner } from './multisig/signing.js';
import { AccountInspector, assertCompleteDetectedConfig } from './inspector.js';
import { ProposalFactory } from './proposal/factory.js';
import { ProposalMetadataCodec } from './proposal/metadata.js';
import { ProposalSignatures } from './proposal/signatures.js';
import {
  importNotesFromProposals as importNotesFromProposalsStandalone,
  type NoteImportOutcome,
} from './proposalNoteImport.js';
import {
  getRawMidenClient,
  getTransactionProver,
  requireMidenRpcEndpoint,
} from './raw-client.js';
import {
  resolveProverConfig,
  type ResolvedProverConfig,
} from './prover/config.js';
import { ProverWorkflow } from './prover/workflow.js';
import {
  resolveRpcConfig,
  type ResolvedRpcConfig,
} from './rpc/config.js';
import { retryRpcRead } from './rpc/retry.js';

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
 * Options shared by the `create*Proposal` family (issue #387). Every optional
 * knob lives in a single trailing options bag, so call sites never need
 * positional `undefined` holes to reach a later option.
 */
export interface CreateProposalOptions {
  /** Proposal nonce; defaults to `Date.now()`. */
  nonce?: number;
}

export interface CreateSignerProposalOptions extends CreateProposalOptions {
  /**
   * New signing threshold. Defaults to the current threshold on add, and to
   * `min(current threshold, remaining signer count)` on remove.
   */
  newThreshold?: number;
}

export interface CreateP2idProposalOptions extends CreateProposalOptions, P2ideHeightOptions {
  /** Visibility of the created note. Defaults to `NoteType.Public` (issue #322). */
  noteType?: NoteType;
}

/**
 * Represents a multisig account with GUARDIAN integration.
 */
const BUILTIN_PROPOSAL_TYPES = new Set<string>([
  'add_signer',
  'remove_signer',
  'change_threshold',
  'update_procedure_threshold',
  'switch_guardian',
  'consume_notes',
  'p2id',
  // Reserved: the SDK's internal bucket name for unmodeled types. A producer
  // must not use it as a custom label, or it would collide with the bucket.
  'custom',
]);

/**
 * Deserialize producer-supplied transaction request bytes, wrapping any failure
 * in a stable message that mirrors the Rust SDK's `deserialize_transaction_request`.
 */
function deserializeTransactionRequest(bytes: Uint8Array): TransactionRequest {
  try {
    return TransactionRequest.deserialize(bytes);
  } catch (err) {
    const detail = err instanceof Error ? err.message : String(err);
    throw new Error(`failed to decode transaction request: ${detail}`);
  }
}

/**
 * Single home for the proposal-nonce default, plus a runtime guard for
 * pre-#387 positional callers. Untyped JS passing the old `nonce` number (or
 * a legacy trailing argument) would otherwise bind it as the options bag and
 * silently fall back to every default — a public note instead of a private
 * one, or the current threshold instead of the requested one — so it must
 * fail loudly instead.
 */
function resolveProposalNonce(
  method: string,
  options: CreateProposalOptions,
  legacyArgs: readonly unknown[] = [],
): number {
  if (typeof options !== 'object' || options === null || legacyArgs.length > 0) {
    throw new Error(
      `${method}: positional optional parameters were replaced by a trailing options object (issue #387); pass { nonce, ... } instead`,
    );
  }
  return options.nonce ?? Date.now();
}

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
  private readonly proverWorkflow: ProverWorkflow;
  private readonly rpcConfig: ResolvedRpcConfig;
  private readonly _accountId: string;
  private readonly midenRpcEndpoint: string;
  private proposals: Map<string, Proposal> = new Map();

  constructor(
    account: Account,
    config: MultisigConfig,
    guardian: GuardianHttpClient,
    signer: Signer,
    midenClient: MidenClient,
    accountId: string | undefined,
    midenRpcEndpoint: string,
    proverConfig?: ResolvedProverConfig,
    rpcConfig?: ResolvedRpcConfig,
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
    this.midenRpcEndpoint = requireMidenRpcEndpoint(midenRpcEndpoint);
    this.rawClientPromise = getRawMidenClient(midenClient, this.midenRpcEndpoint);
    this.proverWorkflow = new ProverWorkflow(
      this.midenClient,
      proverConfig ?? resolveProverConfig(undefined, getTransactionProver(midenClient)),
    );
    this.rpcConfig = rpcConfig ?? resolveRpcConfig(undefined);
  }

  private getMidenRpcEndpoint(): string {
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
   * Resolve the account from the web client's store, falling back to the
   * `account` snapshot when the store has no record.
   *
   * Transaction execution reads the store, and other flows (e.g. consume-notes
   * finalize) update it without refreshing the snapshot, so vault lookups must
   * source from the store to see the same state execution will.
   */
  async getStoreAccount(): Promise<Account> {
    const webClient = await this.getRawClient();
    const stored = await retryRpcRead(
      () => webClient.getAccount(AccountId.fromHex(this._accountId)),
      this.rpcConfig,
    );
    return stored ?? this.account;
  }

  /**
   * Read the current ordered signer public-key commitments from account
   * storage (store-backed state, falling back to the snapshot).
   *
   * Commitments are ordered by signer index as currently stored; indices
   * re-pack when signers are removed, so index 0 is the creation-time first
   * key only until the first membership change. Unlike the
   * `signerCommitments` field, which reflects the config detected at
   * construction / last sync, this reads the account state directly.
   * See `AccountInspector.getSignerPublicKeyCommitments` (issue #306).
   */
  async getSignerPublicKeyCommitments(): Promise<string[]> {
    const account = await this.getStoreAccount();
    return AccountInspector.getSignerPublicKeyCommitments(account);
  }

  /**
   * Read the current guardian public-key commitment from account storage.
   * The guarded-multisig always includes a guardian, so this throws (rather
   * than returning null) when the entry is missing.
   */
  async getGuardianPublicKeyCommitment(): Promise<string> {
    const account = await this.getStoreAccount();
    return AccountInspector.getGuardianPublicKeyCommitment(account);
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
   * Per-procedure threshold overrides whose effective signing ratio is diluted
   * by growing the signer set to `newNumSigners`.
   *
   * Overrides are absolute signature counts, not ratios, and the on-chain
   * `update_signers_and_threshold` procedure does not re-scale them: growing
   * the approver set silently lowers every override's effective signing ratio
   * (a 2-of-2 override becomes 2-of-n). Callers creating a proposal that grows
   * the signer set should surface these overrides and suggest raising them via
   * an update-procedure-threshold proposal alongside the growth.
   *
   * @param newNumSigners - Signer-set size the proposal produces
   * @returns The configured overrides, or an empty list when the set does not grow
   */
  overridesDilutedBySignerGrowth(
    newNumSigners: number,
  ): Array<{ procedure: ProcedureName; threshold: number }> {
    if (newNumSigners <= this.signerCommitments.length) {
      return [];
    }
    return Array.from(this.procedureThresholds.entries()).map(([procedure, threshold]) => ({
      procedure,
      threshold,
    }));
  }

  private warnOnOverrideDilution(newNumSigners: number): void {
    const current = this.signerCommitments.length;
    for (const { procedure, threshold } of this.overridesDilutedBySignerGrowth(newNumSigners)) {
      console.warn(
        `growing the signer set dilutes the ${procedure} threshold override ` +
          `(${threshold}-of-${current} becomes ${threshold}-of-${newNumSigners}); consider raising it ` +
          `via an update-procedure-threshold proposal alongside the signer update`,
      );
    }
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
   * is missing locally) and the GUARDIAN state is safe to import, the local store
   * is overwritten with the GUARDIAN state. When the GUARDIAN is merely *behind*
   * local — e.g. the pushed execution delta has not been canonicalized yet
   * (see OpenZeppelin/guardian#316) — the local state is already ahead and
   * on-chain-verifiable, so it is kept as authoritative. Either way, config is
   * refreshed from the resulting account so callers reading `Multisig.account`
   * (e.g. the UI) observe the current state instead of a stale snapshot.
   */
  async syncState(): Promise<AccountState> {
    const state = await this.fetchState();
    const accountId = AccountId.fromHex(this._accountId);
    const webClient = await this.getRawClient();
    const localAccount = await retryRpcRead(
      () => webClient.getAccount(accountId),
      this.rpcConfig,
    );
    let accountForConfigRefresh: Account | null = localAccount ?? null;

    const guardianCommitment = normalizeHexWord(state.commitment);
    const localCommitment = localAccount
      ? normalizeHexWord(localAccount.to_commitment().toHex())
      : null;

    if (!localAccount || localCommitment !== guardianCommitment) {
      const accountBytes = base64ToUint8Array(state.stateDataBase64);
      const incomingAccount = Account.deserialize(accountBytes);
      if (await this.isSafeToOverwriteLocalState(incomingAccount, localAccount)) {
        await webClient.newAccount(incomingAccount, true);
        accountForConfigRefresh = incomingAccount;
      }
    }

    this.refreshConfigFromAccount(accountForConfigRefresh);

    return state;
  }

  async verifyStateCommitment(): Promise<AccountStateVerificationResult> {
    const accountId = AccountId.fromHex(this._accountId);
    const webClient = await this.getRawClient();
    const localAccount = await retryRpcRead(
      () => webClient.getAccount(accountId),
      this.rpcConfig,
    );

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

  /**
   * Decide whether GUARDIAN-provided state may overwrite the local store.
   *
   * Returns `false` — rather than throwing — when the GUARDIAN state is simply
   * *behind* local (lower nonce). That happens whenever the execution delta the
   * client pushed has not been canonicalized by the GUARDIAN's background worker
   * yet (see OpenZeppelin/guardian#316), or permanently if that candidate was
   * discarded (#312 / #319). In that case the local account is already ahead and
   * is independently verifiable against chain (`verifyStateCommitment`), so it is
   * authoritative and must be kept, not clobbered; the caller keeps local and
   * refreshes config from it.
   *
   * Still throws for genuine divergence: an incoming state at the *same* nonce as
   * local but a different commitment, or an incoming state whose commitment does
   * not match the on-chain commitment.
   */
  private async isSafeToOverwriteLocalState(
    incomingAccount: Account,
    localAccount?: Account,
  ): Promise<boolean> {
    if (localAccount) {
      const localNonce = localAccount.nonce().asInt();
      const incomingNonce = incomingAccount.nonce().asInt();

      if (incomingNonce < localNonce) {
        return false;
      }

      if (incomingNonce === localNonce) {
        throw new Error(
          `Refusing to overwrite local state: incoming nonce ${incomingNonce.toString()} equals local nonce ${localNonce.toString()} but commitments differ for account ${this._accountId}`
        );
      }
    }

    const accountId = AccountId.fromHex(this._accountId);
    const onChainCommitment = await this.getOnChainCommitment(accountId);
    if (!onChainCommitment) {
      return true;
    }

    const incomingCommitment = normalizeHexWord(incomingAccount.to_commitment().toHex());
    if (incomingCommitment !== onChainCommitment) {
      throw new Error(
        `Refusing to overwrite local state: incoming commitment does not match on-chain commitment for account ${this._accountId}`
      );
    }

    return true;
  }

  private async getOnChainCommitment(accountId: AccountId): Promise<string | null> {
    const rpcClient = new RpcClient(new Endpoint(this.getMidenRpcEndpoint()));

    try {
      const accountDetails = await retryRpcRead(
        () => rpcClient.getAccountDetails(accountId),
        this.rpcConfig,
      );
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
      // Fail closed on a partial read: adopting a truncated signer set would
      // let membership proposals rewrite the account without the omitted
      // keys. The catch below keeps the previously validated config instead.
      assertCompleteDetectedConfig(detected);
      this.account = account;
      this.threshold = detected.threshold;
      this.signerCommitments = detected.signerCommitments;
      this.guardianCommitment = detected.guardianCommitment;
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
   * @param options - Optional settings: `nonce`, `newThreshold` (defaults to
   *   current threshold)
   */
  async createAddSignerProposal(
    newCommitment: string,
    options: CreateSignerProposalOptions = {},
    ...legacyArgs: never[]
  ): Promise<Proposal> {
    const proposalNonce = resolveProposalNonce('createAddSignerProposal', options, legacyArgs);
    const webClient = await this.getRawClient();
    const targetThreshold = options.newThreshold ?? this.threshold;
    const targetSignerCommitments = [...this.signerCommitments, newCommitment];
    this.warnOnOverrideDilution(targetSignerCommitments.length);

    const { request, salt } = await buildUpdateSignersTransactionRequest(
      webClient,
      targetThreshold,
      targetSignerCommitments,
      { signatureScheme: this.signer.scheme },
    );

    const { summary, anchor } = await executeForSummary(webClient, this._accountId, request);
    const chainAnchor = chainAnchorToBase64(anchor);
    anchor.free();
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());

    const metadata: ProposalMetadata = {
      chainAnchor,
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
   * @param options - Optional settings: `nonce`, `newThreshold` (defaults to
   *   min of current threshold and new signer count)
   */
  async createRemoveSignerProposal(
    signerToRemove: string,
    options: CreateSignerProposalOptions = {},
    ...legacyArgs: never[]
  ): Promise<Proposal> {
    const proposalNonce = resolveProposalNonce('createRemoveSignerProposal', options, legacyArgs);
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

    const targetThreshold = options.newThreshold ?? Math.min(this.threshold, targetSignerCommitments.length);

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

    const { summary, anchor } = await executeForSummary(webClient, this._accountId, request);
    const chainAnchor = chainAnchorToBase64(anchor);
    anchor.free();
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());

    const metadata: ProposalMetadata = {
      chainAnchor,
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
   * @param options - Optional settings: `nonce`
   */
  async createChangeThresholdProposal(
    newThreshold: number,
    options: CreateProposalOptions = {},
  ): Promise<Proposal> {
    const proposalNonce = resolveProposalNonce('createChangeThresholdProposal', options);
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

    const { summary, anchor } = await executeForSummary(webClient, this._accountId, request);
    const chainAnchor = chainAnchorToBase64(anchor);
    anchor.free();
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());

    const metadata: ProposalMetadata = {
      chainAnchor,
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
    options: CreateProposalOptions = {},
  ): Promise<Proposal> {
    const proposalNonce = resolveProposalNonce('createUpdateProcedureThresholdProposal', options);
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

    const { summary, anchor } = await executeForSummary(webClient, this._accountId, request);
    const chainAnchor = chainAnchorToBase64(anchor);
    anchor.free();
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());
    const action = targetThreshold === 0
      ? `Clear threshold override for ${targetProcedure}`
      : `Set ${targetProcedure} threshold override to ${targetThreshold}`;

    const metadata: ProposalMetadata = {
      chainAnchor,
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
   * @param options - Optional settings: `nonce`
   */
  async createSwitchGuardianProposal(
    newGuardianEndpoint: string,
    newGuardianPubkey: string,
    options: CreateProposalOptions = {},
  ): Promise<Proposal> {
    const proposalNonce = resolveProposalNonce('createSwitchGuardianProposal', options);
    const webClient = await this.getRawClient();
    await this.verifyGuardianEndpointCommitment(newGuardianEndpoint, newGuardianPubkey);

    const { request, salt } = await buildUpdateGuardianTransactionRequest(
      webClient,
      newGuardianPubkey,
      { signatureScheme: this.signer.scheme },
    );

    const { summary, anchor } = await executeForSummary(webClient, this._accountId, request);
    const chainAnchor = chainAnchorToBase64(anchor);
    anchor.free();
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());

    const metadata: ProposalMetadata = {
      chainAnchor,
      proposalType: 'switch_guardian',
      saltHex: salt.toHex(),
      requiredSignatures: this.getEffectiveThreshold('switch_guardian'),
      newGuardianPubkey,
      newGuardianEndpoint,
      description: `Switch GUARDIAN to ${newGuardianEndpoint}`,
    };

    // SwitchGuardian is a regular delta proposal; push it to GUARDIAN so
    // sign/execute (which fetch from GUARDIAN) can find it.
    return this.createProposal(proposalNonce, summaryBase64, metadata);
  }

  /**
   * Create a "consume notes" proposal to consume notes sent to the multisig account.
   *
   * @param noteIds - IDs of the notes to consume (hex strings)
   * @param options - Optional settings: `nonce`
   */
  async createConsumeNotesProposal(
    noteIds: string[],
    options: CreateProposalOptions = {},
  ): Promise<Proposal> {
    const proposalNonce = resolveProposalNonce('createConsumeNotesProposal', options);
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

    const { summary, anchor } = await executeForSummary(webClient, this._accountId, request);
    const chainAnchor = chainAnchorToBase64(anchor);
    anchor.free();
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());

    const metadata: ProposalMetadata = {
      chainAnchor,
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
   * @param options - Optional settings: `nonce`; `noteType` selects the created
   *   note's visibility (defaults to `NoteType.Public`, issue #322);
   *   `reclaimHeight`/`timelockHeight` build a P2IDE note (issue #366)
   */
  async createP2idProposal(
    recipientId: string,
    faucetId: string,
    amount: bigint,
    options: CreateP2idProposalOptions = {},
    ...legacyArgs: never[]
  ): Promise<Proposal> {
    const proposalNonce = resolveProposalNonce('createP2idProposal', options, legacyArgs);
    const webClient = await this.getRawClient();
    if (amount <= 0n) {
      throw new Error('Amount must be greater than 0');
    }

    // Forward everything but the nonce, so a note option added to
    // CreateP2idProposalOptions can't be silently dropped before the builder.
    const { nonce: _nonce, ...noteOptions } = options;
    const { request, salt } = buildP2idTransactionRequest(
      this._accountId,
      recipientId,
      faucetId,
      amount,
      noteOptions,
    );

    const { summary, anchor } = await executeForSummary(webClient, this._accountId, request);
    const chainAnchor = chainAnchorToBase64(anchor);
    anchor.free();
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());

    const metadata: ProposalMetadata = {
      chainAnchor,
      proposalType: 'p2id',
      saltHex: salt.toHex(),
      requiredSignatures: this.getEffectiveThreshold('p2id'),
      recipientId,
      faucetId,
      amount: amount.toString(),
      // Omitted for public notes so the wire shape matches pre-#322 proposals.
      noteType: p2idNoteTypeToMetadata(options.noteType),
      // Omitted when absent so plain-P2ID payloads keep the pre-#366 wire shape.
      reclaimHeight: options.reclaimHeight,
      timelockHeight: options.timelockHeight,
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
        // Miden 0.15: InputNoteRecord.id() is `NoteId | undefined`; skip id-less records.
        const id = inputNote.id();
        if (id === undefined) {
          continue;
        }
        const noteId = id.toString();
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
   * Export a note created by this multisig account as serialized note-file
   * bytes for out-of-band delivery.
   *
   * A private note publishes only its commitment on chain, so the recipient
   * can never learn its contents via sync; the sender must hand them the
   * bytes produced here, which they load with {@link importNoteFromBytes}.
   *
   * The note must be an output note of this client (created by a transaction
   * this client executed). When the note's on-chain inclusion proof is
   * already known (after a post-commit sync) the full note with proof is
   * exported; otherwise the note details are exported and the importer's
   * client tracks the note until it commits on chain.
   *
   * @param noteId - ID of the note to export (hex string)
   * @returns Serialized note file bytes
   */
  async exportNoteToBytes(noteId: string): Promise<Uint8Array> {
    const webClient = await this.getRawClient();
    const trimmedNoteId = noteId.trim();

    let record;
    try {
      record = await webClient.getOutputNote(trimmedNoteId);
    } catch (err) {
      const detail = err instanceof Error ? err.message : String(err);
      throw new Error(
        `Output note ${trimmedNoteId} not found in the local store; only notes created by this client can be exported: ${detail}`,
      );
    }
    if (!record) {
      throw new Error(
        `Output note ${trimmedNoteId} not found in the local store; only notes created by this client can be exported`,
      );
    }

    const format = record.inclusionProof()
      ? NoteExportFormat.Full
      : NoteExportFormat.Details;
    const noteFile = await webClient.exportNoteFile(trimmedNoteId, format);
    return noteFile.serialize();
  }

  /**
   * Export a note created by this multisig account as a note file downloaded
   * by the browser. Browser-only convenience over
   * {@link exportNoteToBytes}; use that method directly in non-DOM
   * environments.
   *
   * @param noteId - ID of the note to export (hex string)
   * @param filename - Download filename; defaults to `note_<id>.mno`
   */
  async exportNoteToFile(noteId: string, filename?: string): Promise<void> {
    if (typeof document === 'undefined') {
      throw new Error('exportNoteToFile requires a browser environment; use exportNoteToBytes instead');
    }

    const trimmedNoteId = noteId.trim();
    const noteBytes = await this.exportNoteToBytes(trimmedNoteId);

    const blob = new Blob([noteBytes as BlobPart], { type: 'application/octet-stream' });
    const url = URL.createObjectURL(blob);
    try {
      const anchor = document.createElement('a');
      anchor.href = url;
      anchor.download = filename ?? `note_${trimmedNoteId}.mno`;
      anchor.click();
    } finally {
      URL.revokeObjectURL(url);
    }
  }

  /**
   * Import a note file received out-of-band so the note can be
   * consumed by this multisig account.
   *
   * Sync the Miden client with the network afterwards so the note's on-chain
   * commitment is tracked and the note shows up in {@link getConsumableNotes};
   * it can then be consumed via {@link createConsumeNotesProposal}.
   *
   * @param noteBytes - Serialized note file bytes produced by
   *   {@link exportNoteToBytes}
   * @returns The note ID when the file carries one, or the note's details
   *   commitment for a details-only file
   */
  async importNoteFromBytes(noteBytes: Uint8Array): Promise<string> {
    const webClient = await this.getRawClient();

    let noteFile: NoteFile;
    try {
      noteFile = NoteFile.deserialize(noteBytes);
    } catch (err) {
      const detail = err instanceof Error ? err.message : String(err);
      throw new Error(`failed to decode note file: ${detail}`);
    }

    return webClient.importNoteFile(noteFile);
  }

  /**
   * Import a note file received out-of-band from a browser
   * `File`/`Blob` (e.g. a file-input selection). See
   * {@link importNoteFromBytes} for the returned identifier semantics.
   */
  async importNoteFromFile(file: Blob): Promise<string> {
    const noteBytes = new Uint8Array(await file.arrayBuffer());
    return this.importNoteFromBytes(noteBytes);
  }

  /**
   * Import the notes embedded in this account's pending v2 consume-notes
   * proposals into the local Miden store (issue #415) — the recovery
   * convenience over the standalone
   * {@link importNotesFromProposalsStandalone | importNotesFromProposals},
   * reusing this client's Miden RPC endpoint and retry configuration
   * instead of taking them as parameters.
   *
   * When `proposals` is omitted, the pending proposals are synced from
   * GUARDIAN first. See the standalone function for per-note outcome
   * semantics; run a sync afterwards so imported notes are verified.
   */
  async importNotesFromProposals(
    proposals?: ReadonlyArray<Pick<Proposal, 'id' | 'metadata'>>,
  ): Promise<NoteImportOutcome[]> {
    const source = proposals ?? (await this.syncProposals());
    return importNotesFromProposalsStandalone(this.midenClient, source, {
      midenRpcEndpoint: this.getMidenRpcEndpoint(),
      rpc: { retry: { maxAttempts: this.rpcConfig.maxAttempts } },
    });
  }

  /**
   * Compute the ID of the note a P2ID proposal will create when executed.
   *
   * The P2ID note is rebuilt deterministically from the proposal salt, so the
   * ID is known ahead of execution. For a private P2ID this is the ID to pass
   * to {@link exportNoteToBytes} after executing, so the note file can be delivered
   * to the recipient out-of-band.
   *
   * The note ID remains deterministic from the proposal metadata and salt.
   */
  async getP2idNoteId(proposal: Proposal): Promise<string> {
    const metadata = proposal.metadata;
    if (
      metadata.proposalType !== 'p2id' ||
      !metadata.recipientId ||
      !metadata.faucetId ||
      !metadata.amount ||
      !metadata.saltHex
    ) {
      throw new Error('getP2idNoteId requires a P2ID proposal with recipient, faucet, amount, and salt metadata');
    }

    const note = buildP2idNoteFromMetadata(
      this._accountId,
      metadata.recipientId,
      metadata.faucetId,
      BigInt(metadata.amount),
      parseP2idNoteType(metadata.noteType),
      metadata.saltHex,
      { reclaimHeight: metadata.reclaimHeight, timelockHeight: metadata.timelockHeight },
    );
    return note.id().toString();
  }

  /**
   * Sign a proposal.
   *
   * The proposalId is the tx_summary commitment hex, which is what gets signed.
   * This matches the Rust client behavior where proposal.id == tx_summary.to_commitment().
   *
  * @param proposalId - The proposal commitment/ID (this is also what gets signed)
  */
  /**
   * Request abandonment of a pending canonicalization candidate whose
   * transaction will never land on-chain (issue #319) — e.g. after an
   * approved transaction died client-side (RPC submit failure, prover
   * timeout, crash).
   *
   * Records an abandon *intent* on GUARDIAN: the account stays locked
   * until the guardian's canonicalization worker confirms over a short
   * quarantine (typically well under a minute) that the transaction did
   * not land, then releases the account. Poll {@link abandonStatus} for
   * the resolution.
   *
   * `nonce` pins the exact candidate to release; it is the nonce the
   * proposal was pushed with. Retries are idempotent and preserve the
   * original request timestamp. Refused with `GUARDIAN_CANDIDATE_LANDED`
   * (409) when the transaction actually landed.
   */
  async abandonCandidate(nonce: number): Promise<AbandonCandidateResponse> {
    return this.guardian.abandonCandidate(this._accountId, nonce);
  }

  /**
   * Poll the resolution of an abandon request made with
   * {@link abandonCandidate}: `'waiting'` while the quarantine runs,
   * `'landed'` if the transaction landed after all, `'abandoned'` once
   * the account is released, `'unexpected'` for any state no abandon
   * flow produces.
   */
  async abandonStatus(nonce: number): Promise<AbandonStatus> {
    return this.guardian.abandonStatus(this._accountId, nonce);
  }

  /**
   * Fetch one page of this account's canonical delta history
   * from GUARDIAN (issue #413), newest-first by nonce, with decoded
   * input/output note summaries. Pass `options.cursor` from a previous
   * page's `nextCursor` to resume; an absent `nextCursor` means the
   * feed is exhausted. Served while the account is paused. Only
   * transactions pushed through GUARDIAN appear — history of
   * transactions executed elsewhere is not visible to it.
   */
  async deltaHistory(options: HistoryOptions = {}): Promise<HistoryPage> {
    return this.guardian.getDeltaHistory(this._accountId, options);
  }

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

    // Execute at the proposal's anchored reference block, so the summary the
    // cosigners signed reproduces exactly. The anchor was already checked
    // against the summary's block commitment during binding verification.
    const accountId = AccountId.fromHex(this._accountId);
    const anchor = this.requireProposalAnchor(proposalId, proposal.metadata);
    try {
      await this.proverWorkflow.submitAt(accountId, finalRequest, anchor);
    } finally {
      anchor.free();
    }

    if (metadata.proposalType === 'switch_guardian') {
      if (!metadata.newGuardianEndpoint || !metadata.newGuardianPubkey) {
        throw new Error('Switch GUARDIAN proposal metadata is incomplete after execution');
      }

      // Canonicalize the executed delta on the pre-switch GUARDIAN (clears the
      // pending proposal). Must run before `this.guardian` is repointed below.
      // Best-effort: an unreachable old GUARDIAN must not block the switch, so
      // errors are swallowed (mirrors the Rust execute path).
      try {
        const normalizedProposalId = normalizeHexWord(proposal.id);
        const switchDelta = await this.guardian.getDeltaProposal(
          this._accountId,
          normalizedProposalId,
        );
        await this.guardian.pushDelta({
          ...switchDelta,
          deltaPayload: switchDelta.deltaPayload.txSummary,
        });
      } catch (error) {
        // Best-effort — see above — but the failure must be visible: a
        // silently lost push leaves the pre-switch GUARDIAN serving this
        // account (split-brain, issue #305) with nothing to diagnose by.
        console.warn(
          'SwitchGuardian delta push to the pre-switch GUARDIAN failed; it ' +
            'will keep serving this account until reconciliation',
          error,
        );
      }

      try {
        const webClient = await this.getRawClient();
        await retryRpcRead(() => webClient.syncState(), this.rpcConfig);

        const updatedAccount = await retryRpcRead(
          () => webClient.getAccount(accountId),
          this.rpcConfig,
        );
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

  /**
   * Submit an integration-built transaction (advice already injected). Mirrors
   * the Rust `submit_transaction`; used by the custom proposal producer flow
   * after `prepareCustomExecution` rebuilds its request with the returned advice.
   * The transaction is executed at the proposal's anchored reference block,
   * since the collected signatures only authorize the summary produced there.
   */
  async submitTransaction(proposalId: string, request: TransactionRequest): Promise<void> {
    const normalizedProposalId = normalizeHexWord(proposalId);
    const delta = await this.guardian.getDeltaProposal(this._accountId, normalizedProposalId);
    const existing = this.getLocalProposal(proposalId);
    const proposal = this.proposalFactory().fromDelta(
      delta,
      normalizedProposalId,
      existing?.metadata,
      existing?.signatures ?? [],
    );

    const anchor = this.requireProposalAnchor(proposalId, proposal.metadata);
    try {
      const anchorCommitment = normalizeHexWord(anchor.commitment().toHex());
      const txSummary = TransactionSummary.deserialize(
        base64ToUint8Array(delta.deltaPayload.txSummary.data),
      );
      const summaryBlockCommitment = normalizeHexWord(txSummary.blockCommitment().toHex());
      if (anchorCommitment !== summaryBlockCommitment) {
        throw new Error(
          `Proposal ${proposalId} chain anchor does not match the block commitment bound into its tx_summary`,
        );
      }

      await this.proverWorkflow.submitAt(AccountId.fromHex(this._accountId), request, anchor);
    } finally {
      anchor.free();
    }
  }

  /**
   * Create a proposal from a producer-built transaction the SDK does not model.
   * `transactionRequestBytes` is a serialized TransactionRequest;
   * `proposalType` is a free-form, non-empty label that must not collide with a
   * built-in type. The integration keeps its own recipe to execute later via
   * `prepareCustomExecution`.
   */
  async createCustomProposal(
    transactionRequestBytes: Uint8Array,
    proposalType: string,
    options: CreateProposalOptions = {},
  ): Promise<Proposal> {
    const proposalNonce = resolveProposalNonce('createCustomProposal', options);
    const label = proposalType.trim().toLowerCase();
    if (label.length === 0) {
      throw new Error('proposalType must not be empty');
    }
    if (!/^[a-z0-9_]+$/.test(label)) {
      throw new Error(
        `proposalType '${label}' must be lowercase snake_case ([a-z0-9_]): no spaces, hyphens, or other characters`,
      );
    }
    if (BUILTIN_PROPOSAL_TYPES.has(label)) {
      throw new Error(
        `'${label}' is a built-in proposal type; use the typed proposal API instead`,
      );
    }

    const webClient = await this.getRawClient();
    const request = deserializeTransactionRequest(transactionRequestBytes);
    const { summary, anchor } = await executeForSummary(webClient, this._accountId, request);
    const chainAnchor = chainAnchorToBase64(anchor);
    anchor.free();
    const summaryBase64 = uint8ArrayToBase64(summary.serialize());

    const metadata: ProposalMetadata = {
      chainAnchor,
      proposalType: 'custom',
      description: '',
      rawProposalType: label,
      requiredSignatures: this.getEffectiveThreshold('custom'),
    };

    return this.createProposal(proposalNonce, summaryBase64, metadata);
  }

  /**
   * Assemble the validated execution advice (cosigner signatures + GUARDIAN
   * acknowledgment) for a ready custom proposal, so an integration can rebuild
   * its transaction with its own recipe and submit.
   *
   * `transactionRequestBytes` is the serialized transaction request; it is used only to verify
   * (binding check) that it reproduces the signed commitment, before the
   * acknowledgment is requested. Returns the advice the integration folds into
   * its rebuilt transaction (`builder.extendAdviceMap(advice)`).
   */
  async prepareCustomExecution(
    proposalId: string,
    transactionRequestBytes: Uint8Array,
  ): Promise<AdviceMap> {
    const normalizedProposalId = normalizeHexWord(proposalId);
    const delta = await this.guardian.getDeltaProposal(this._accountId, normalizedProposalId);
    const existing = this.getLocalProposal(proposalId);
    const proposal = this.proposalFactory().fromDelta(
      delta,
      normalizedProposalId,
      existing?.metadata,
      existing?.signatures ?? [],
    );

    if (proposal.metadata.proposalType !== 'custom') {
      throw new Error(
        'prepareCustomExecution is only for custom proposals; use executeProposal for built-in types',
      );
    }

    const effectiveThreshold = this.getEffectiveThreshold('custom');
    const signaturesForExecution = new ProposalSignatures(
      proposal.signatures,
      this.signerCommitments,
      `Invalid proposal signatures for ${proposalId}`,
    ).entries();
    if (signaturesForExecution.length < effectiveThreshold) {
      throw new Error(
        `Proposal is not ready for execution: have ${signaturesForExecution.length} of ${effectiveThreshold} required signatures.`,
      );
    }

    const txSummary = TransactionSummary.deserialize(
      base64ToUint8Array(delta.deltaPayload.txSummary.data),
    );
    const signedCommitmentHex = normalizeHexWord(txSummary.toCommitment().toHex());

    const bindingRequest = deserializeTransactionRequest(transactionRequestBytes);

    // Probe at the proposal's anchored reference block: the signed summary
    // binds that block's commitment, so probing at the local sync height would
    // never reproduce it. The anchor arrives from an untrusted party via
    // GUARDIAN, so its block commitment is checked against the signed summary
    // before executing against it.
    const anchor = this.requireProposalAnchor(proposalId, proposal.metadata);
    let derivedCommitmentHex: string;
    try {
      const anchorCommitment = normalizeHexWord(anchor.commitment().toHex());
      const summaryBlockCommitment = normalizeHexWord(txSummary.blockCommitment().toHex());
      if (anchorCommitment !== summaryBlockCommitment) {
        throw new Error(
          `Custom proposal ${proposalId} chain anchor does not match the block commitment bound into its tx_summary`,
        );
      }
      const webClient = await this.getRawClient();
      const derived = await executeForSummaryAt(webClient, this._accountId, bindingRequest, anchor);
      derivedCommitmentHex = normalizeHexWord(derived.toCommitment().toHex());
    } finally {
      anchor.free();
    }
    if (derivedCommitmentHex !== signedCommitmentHex) {
      throw new Error(
        `Custom proposal binding mismatch: expected ${signedCommitmentHex}, got ${derivedCommitmentHex}`,
      );
    }

    return this.assembleCustomAdvice(
      proposalId,
      signaturesForExecution,
      signedCommitmentHex,
      delta,
    );
  }

  private async assembleCustomAdvice(
    proposalId: string,
    signaturesForExecution: ProposalSignatureEntry[],
    normalizedTxCommitmentHex: string,
    delta: DeltaObject,
  ): Promise<AdviceMap> {
    const normalizedSignerCommitments = new Set(
      this.signerCommitments.map((commitment) => normalizeHexWord(commitment)),
    );
    const adviceMap = new AdviceMap();
    const adviceMapKeys = new Set<string>();
    const createTxCommitmentWord = (): Word => Word.fromHex(normalizedTxCommitmentHex);

    for (const cosignerSig of signaturesForExecution) {
      let signerCommitmentHex = normalizeHexWord(cosignerSig.signerId);
      const ecdsaPublicKey =
        cosignerSig.signature.scheme === 'ecdsa' ? cosignerSig.signature.publicKey : undefined;

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
      if (cosignerSig.signature.scheme === 'ecdsa' && ecdsaPublicKey) {
        assertEcdsaSignatureRecoverable(
          cosignerSig.signature.signature,
          normalizedTxCommitmentHex,
          ecdsaPublicKey,
        );
      }
      const { key, values } = buildSignatureAdviceEntry(
        signerCommitment,
        createTxCommitmentWord(),
        signature,
      );
      const keyHex = normalizeHexWord(key.toHex());
      if (adviceMapKeys.has(keyHex)) {
        throw new Error(`Duplicate advice-map key detected for proposal ${proposalId}`);
      }
      adviceMapKeys.add(keyHex);
      adviceMap.insert(key, new FeltArray(values));
    }

    const executionDelta = { ...delta, deltaPayload: delta.deltaPayload.txSummary };
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
    if (ackScheme === 'ecdsa' && ackPubkey) {
      assertEcdsaSignatureRecoverable(ackSigHex, normalizedTxCommitmentHex, ackPubkey);
    }
    const { key: ackKey, values: ackValues } = buildSignatureAdviceEntry(
      guardianCommitment,
      createTxCommitmentWord(),
      ackSignature,
    );
    const ackKeyHex = normalizeHexWord(ackKey.toHex());
    if (adviceMapKeys.has(ackKeyHex)) {
      throw new Error(
        `Duplicate advice-map key detected for GUARDIAN acknowledgment in proposal ${proposalId}`,
      );
    }
    adviceMapKeys.add(ackKeyHex);
    adviceMap.insert(ackKey, new FeltArray(ackValues));

    return adviceMap;
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
    // Reject custom proposals before any advice assembly or GUARDIAN ack push:
    // the SDK cannot rebuild an opaque custom transaction, and the rejection
    // must stay side-effect free (mirrors the Rust early guard in execute_proposal).
    if (metadata.proposalType === 'custom') {
      throw new Error(
        'Cannot execute a custom proposal via executeProposal; use prepareCustomExecution to ' +
          'get the cosigner + GUARDIAN advice, then submitTransaction with your rebuilt request (issue #266).',
      );
    }
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
    const saltHex = summarySalt(txSummary).toHex();
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
      if (cosignerSig.signature.scheme === 'ecdsa' && ecdsaPublicKey) {
        assertEcdsaSignatureRecoverable(
          cosignerSig.signature.signature,
          normalizedTxCommitmentHex,
          ecdsaPublicKey,
        );
      }
      const { key, values } = buildSignatureAdviceEntry(
        signerCommitment,
        createTxCommitmentWord(),
        signature,
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
      if (ackScheme === 'ecdsa' && ackPubkey) {
        assertEcdsaSignatureRecoverable(ackSigHex, normalizedTxCommitmentHex, ackPubkey);
      }
      const { key: ackKey, values: ackValues } = buildSignatureAdviceEntry(
        guardianCommitment,
        createTxCommitmentWord(),
        ackSignature,
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

    const summary = TransactionSummary.deserialize(base64ToUint8Array(proposal.txSummary));

    // The anchor arrives from an untrusted party via GUARDIAN, so check its
    // block commitment against the one bound into the signed summary before
    // anything executes against it. `ChainAnchor.deserialize` already enforced
    // internal header/chain consistency.
    const anchor = this.requireProposalAnchor(proposal.id, proposal.metadata);
    try {
      const anchorCommitment = normalizeHexWord(anchor.commitment().toHex());
      const summaryBlockCommitment = normalizeHexWord(summary.blockCommitment().toHex());
      if (anchorCommitment !== summaryBlockCommitment) {
        throw new Error(
          `Invalid proposal: chain anchor does not match the block commitment bound into the tx_summary for ${proposal.id}`,
        );
      }

      if (proposal.metadata.proposalType === 'custom') {
        // Custom proposals have no per-type reconstruction recipe;
        // the id ↔ tx_summary commitment match above is the only available
        // integrity guarantee for an opaque proposal.
        return txSummaryCommitment;
      }

      if (proposal.metadata.proposalType === 'switch_guardian') {
        // Re-execution would mutate the WASM account twice. The proposal ID and
        // guardian endpoint commitment provide the binding checks for this type.
        return txSummaryCommitment;
      }

      const salt = proposal.metadata.saltHex
        ? Word.fromHex(normalizeHexWord(proposal.metadata.saltHex))
        : summarySalt(summary);

      const request = await this.buildTransactionRequestFromMetadata(proposal.metadata, salt);
      const webClient = await this.getRawClient();
      const reconstructed = await executeForSummaryAt(webClient, this._accountId, request, anchor);
      const reconstructedCommitment = normalizeHexWord(reconstructed.toCommitment().toHex());

      if (reconstructedCommitment !== txSummaryCommitment) {
        throw new Error(`Invalid proposal: metadata does not match tx_summary for ${proposal.id}`);
      }

      return txSummaryCommitment;
    } finally {
      anchor.free();
    }
  }

  /**
   * Decodes a proposal's chain anchor. Throws when absent: a proposal without
   * an anchor was created at an unknown reference block, so its signed summary
   * cannot be reproduced, verified, or executed. The caller owns the returned
   * anchor and must `free()` it once done.
   */
  private requireProposalAnchor(proposalId: string, metadata: ProposalMetadata): ChainAnchor {
    if (!metadata.chainAnchor) {
      throw new Error(
        `Proposal ${proposalId} has no chain anchor; it was created without ` +
          'chain-anchored execution and its signed summary cannot be reproduced ' +
          'at the original reference block',
      );
    }
    return chainAnchorFromBase64(metadata.chainAnchor);
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
          {
            salt,
            signatureAdviceMap,
            noteType: parseP2idNoteType(metadata.noteType),
            reclaimHeight: metadata.reclaimHeight,
            timelockHeight: metadata.timelockHeight,
          }
        );
        return request;
      }
      case 'custom':
        throw new Error(
          `Cannot build a transaction for a custom proposal type: ${metadata.rawProposalType ?? 'custom'}`,
        );
    }
  }

}
