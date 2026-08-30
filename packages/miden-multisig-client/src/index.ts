/**
 * @openzeppelin/miden-multisig-client
 *
 * TypeScript SDK for Miden multisig accounts with Guardian integration.
 *
 * @example
 * ```typescript
 * import {
 *   MultisigClient,
 *   FalconSigner,
 * } from '@openzeppelin/miden-multisig-client';
 * import { MidenClient, AuthSecretKey } from '@miden-sdk/miden-sdk';
 *
 * const midenClient = await MidenClient.createDevnet();
 * const secretKey = AuthSecretKey.rpoFalconWithRNG(undefined);
 *
 * // Store in miden-sdk's keystore
 * await midenClient.keystore.insert(secretKey.publicKey(), secretKey);
 *
 * // Create a signer
 * const signer = new FalconSigner(secretKey);
 *
 * // Create multisig client. Both endpoints are required; midenRpcEndpoint
 * // must point at the same network as the injected MidenClient.
 * const client = new MultisigClient(midenClient, {
 *   guardianEndpoint: 'http://localhost:3000',
 *   midenRpcEndpoint: 'https://rpc.devnet.miden.io',
 *   prover: {
 *     url: 'https://prover.example',
 *     retry: { maxAttempts: 4 },
 *   },
 * });
 *
 * // Get GUARDIAN pubkey for config
 * const guardianCommitment = await client.guardianClient.getPubkey();
 *
 * // Create multisig account
 * const config = { threshold: 2, signerCommitments: [signer.commitment, ...], guardianCommitment };
 * const multisig = await client.create(config, signer);
 *
 * // Register on GUARDIAN and work with proposals
 * await multisig.registerOnGuardian();
 * await multisig.syncProposals();
 * ```
 */

export {
  MultisigClient,
  type MultisigClientConfig,
  type RecoveredAccount,
} from './client.js';
export type {
  TransportRecoveryReport,
  TransportRecoveryStatus,
} from './recovery/transportDrain.js';
export type { ProverConfig, ProverRetryPolicy } from './prover/config.js';
export type { RpcConfig, RpcRetryPolicy } from './rpc/config.js';
export { lookupAuthDigest } from './lookupAuth.js';
export {
  Multisig,
  type AccountState,
  type CreateProposalOptions,
  type CreateSignerProposalOptions,
  type CreateP2idProposalOptions,
} from './multisig.js';
export { AccountInspector, type DetectedMultisigConfig, type VaultBalance } from './inspector.js';
export {
  chainAnchorFromBase64,
  chainAnchorToBase64,
  executeForSummary,
  executeForSummaryAt,
  summaryAuthArg,
  buildUpdateSignersTransactionRequest,
  buildUpdateProcedureThresholdTransactionRequest,
  buildUpdateGuardianTransactionRequest,
  buildConsumeNotesTransactionRequest,
  buildP2idTransactionRequest,
  parseP2idNoteType,
  p2idNoteTypeToMetadata,
  // Exported for CUSTOM proposals. The typed builders above commit fee conversion info
  // themselves, but `createCustomProposal` executes caller-supplied request bytes as
  // given and never touches their auth args -- so on a fee-charging chain the caller has
  // to make the same commitment, and until now had no supported way to. Without it a
  // custom proposal aborts in `fee::pay_fee` with ERR_FEE_CONVERSION_INFO_MISSING.
  //
  // Setting the auth arg directly is the only route here, and the right one. Rust's
  // `TransactionRequestBuilder::fee_conversion_info` additionally flags the request as
  // DECLARING conversion info, which makes miden-client classify the account's auth
  // component and reject a guarded multisig; the Rust SDK avoids it for that reason. The
  // wasm build installed here exposes no equivalent -- only `withAuthArg` and
  // `extendAdviceMap` -- so both SDKs converge on the same two calls. That is a fact
  // about this build, not the binding surface: the pinned websdk source does export
  // `withFeeConversionInfo`, which an integrator on a stock install must still avoid
  // on a guarded account.
  applyAuthArg,
  resolveAuthArg,
  nativeConversionInfo,
  feeAuthArg,
  type P2idTransactionOptions,
  type P2ideHeightOptions,
} from './transaction.js';
export type { SignatureOptions } from './transaction/options.js';

export { GuardianHttpClient, GuardianHttpError } from '@openzeppelin/guardian-client';
export type { GuardianErrorMeta } from '@openzeppelin/guardian-client';
// Typed error-code vocabulary (issue #318): branch on GuardianErrorCode,
// never on message text; unknown wire codes surface via rawCode.
export {
  GUARDIAN_ERROR_CODES,
  isGuardianErrorCode,
  normalizeGuardianErrorCode,
} from '@openzeppelin/guardian-client';
export type { GuardianErrorCode } from '@openzeppelin/guardian-client';
export type {
  HistoryDecodeSection,
  HistoryDecodeWarning,
  HistoryEntry,
  HistoryEntryStatus,
  HistoryNote,
  HistoryNoteAsset,
  HistoryNoteTag,
  HistoryNoteVisibility,
  HistoryOptions,
  HistoryPage,
} from '@openzeppelin/guardian-client';

// Codeless transport-failure classification (feature 009, User Story 3).
export { isLikelyNetworkError, toUserFacingError } from './connectivity.js';
export type { ConnectivityCategory, UserFacingError } from './connectivity.js';

// The wallet-facing recovery flow (`Multisig.recoverNotes`) and its report
// types. The individual strategies are internal — the flow is the one entry
// point, so callers cannot accidentally skip the required context (tracked
// account, synced store) or the final verifying sync.
export type {
  NoteImportOutcome,
  NoteImportSource,
  NoteImportStatus,
} from './recovery/proposalNoteImport.js';
export type { BlockRange, PublicBackfillReport } from './recovery/publicNoteBackfill.js';
export type {
  NoteRecoveryReport,
  RecoverNotesOptions,
  RecoveryStep,
  RecoveryStepProblem,
} from './recovery/recoverNotes.js';

export {
  FalconSigner,
  EcdsaSigner,
  ParaSigner,
  MidenWalletSigner,
  type ParaSigningContext,
  type WalletSigningContext,
} from './signer.js';
export { PublicKeyFormat } from './utils/key.js';
export { EcdsaFormat } from './utils/ecdsa.js';
export { tryComputeEcdsaCommitmentHex } from './utils/signature.js';

export {
  createMultisigAccount,
  validateMultisigConfig,
  buildMultisigStorageSlots,
  buildGuardianStorageSlots,
  storageLayoutBuilder,
  StorageLayoutBuilder,
} from './account/index.js';

export {
  CONSUME_NOTES_METADATA_VERSION_V2,
  MAX_CONSUME_NOTES_METADATA_BYTES,
  isConsumeNotesV1,
  isConsumeNotesV2,
  isP2idNoteVisibility,
  type P2idNoteVisibility,
  MAX_P2IDE_BLOCK_HEIGHT,
  parseP2ideHeight,
} from './types/proposal.js';

export {
  LEGACY_CONSUME_NOTES_ENABLED,
} from './multisig/config.js';

export {
  type ConsumeNotesErrorCode,
  NoteBindingMismatchError,
  UnsupportedMetadataVersionError,
  ConsumeNotesMetadataOversizeError,
  LegacyConsumeNotesNoteMissingError,
} from './multisig/consumeNotesErrors.js';

export {
  type AuthArgErrorCode,
  FeeFaucetAnchorMismatchError,
  ProposalAuthArgUnresolvableError,
  ProposalSaltMalformedError,
} from './multisig/authArgErrors.js';

export {
  noteToBase64,
  noteFromBase64,
} from './utils/encoding.js';

export {
  PROCEDURE_ROOTS,
  getProcedureRoot,
  isProcedureName,
  getProcedureNames,
  type ProcedureName,
} from './procedures.js';

export type {
  // Account types
  MultisigAccountState,
  MultisigConfig,
  CreateAccountResult,
  ProcedureThreshold,

  // Proposal types
  Proposal,
  ProposalStatus,
  ProposalSignatureEntry,
  ProposalMetadata,
  ProposalType,
  ExportedProposal,
  ExportedTransactionProposal,
  SignTransactionProposalParams,
  TransactionProposal,
  TransactionProposalSignature,
  TransactionProposalStatus,

  // Transaction types
  TransactionType,

  // Note types
  ConsumableNote,
  NoteAsset,

  // Signature types
  FalconSignature,
  Signer,
  SignatureScheme,

  // GUARDIAN API types
  AuthConfig,
  DeltaObject,
  DeltaStatus,
  StateObject,
  CosignerSignature,

  // Request/Response types
  ConfigureRequest,
  ConfigureResponse,
  DeltaProposalRequest,
  DeltaProposalResponse,
  ProposalsResponse,
  PubkeyResponse,
  SignProposalRequest,
} from './types.js';
