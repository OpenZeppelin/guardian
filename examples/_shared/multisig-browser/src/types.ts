import type { AuthSecretKey } from '@miden-sdk/miden-sdk';
import type {
  AccountState,
  ConsumableNote,
  DetectedMultisigConfig,
  ProcedureName,
  Proposal,
  ProposalMetadata,
  ProposalSignatureEntry,
  SignatureScheme,
  Signer,
  VaultBalance,
} from '@openzeppelin/miden-multisig-client';

export interface LocalSignerInfo {
  commitment: string;
  secretKey: AuthSecretKey;
}

export interface SignerInfo {
  falcon: LocalSignerInfo;
  ecdsa: LocalSignerInfo;
  activeScheme: SignatureScheme;
}

export type WalletSource = 'local' | 'para' | 'miden-wallet';

export interface ExternalWalletState {
  source: WalletSource;
  connected: boolean;
  publicKey: string | null;
  commitment: string | null;
  scheme: SignatureScheme | null;
}

export interface ResolvedSigner {
  commitment: string;
  signatureScheme: SignatureScheme;
  signerInstance: Signer;
  walletSource: WalletSource;
}

export interface SerializedVaultBalance {
  faucetId: string;
  amount: string;
}

export interface SerializedDetectedMultisigConfig {
  threshold: number;
  numSigners: number;
  signerCommitments: string[];
  guardianEnabled: boolean;
  guardianCommitment: string | null;
  vaultBalances: SerializedVaultBalance[];
  procedureThresholds: Array<{ procedure: ProcedureName; threshold: number }>;
}

export interface SerializedAccountState extends AccountState {}

export interface SerializedNoteAsset {
  faucetId: string;
  amount: string;
}

export interface SerializedConsumableNote {
  id: string;
  assets: SerializedNoteAsset[];
}

export interface SerializedProposalSignatureEntry {
  signerId: string;
  timestamp: string;
  signature: {
    scheme: ProposalSignatureEntry['signature']['scheme'];
    signature: string;
    publicKey?: string;
  };
}

export interface SerializedProposal {
  id: string;
  accountId: string;
  nonce: number;
  status: Proposal['status'];
  txSummary: string;
  signatures: SerializedProposalSignatureEntry[];
  metadata: ProposalMetadata;
}

export interface SerializedExternalWalletState {
  source: ExternalWalletState['source'];
  connected: boolean;
  publicKey: string | null;
  commitment: string | null;
  scheme: SignatureScheme | null;
}

export interface SerializedSignerInfo {
  activeScheme: SignatureScheme;
  falconCommitment: string;
  ecdsaCommitment: string;
}

export type SmokeBootStatus = 'idle' | 'initializing' | 'ready' | 'error';

export interface BrowserSessionSnapshot {
  browserLabel: string | null;
  initialized: boolean;
  bootStatus: SmokeBootStatus;
  bootError: string | null;
  guardianEndpoint: string | null;
  midenRpcEndpoint: string | null;
  signerSource: WalletSource | null;
  signatureScheme: SignatureScheme | null;
  guardianPubkey: string | null;
  localSigners: SerializedSignerInfo | null;
  para: SerializedExternalWalletState;
  midenWallet: SerializedExternalWalletState;
  multisig: {
    accountId: string;
    signerCommitment: string;
    threshold: number;
    signerCommitments: string[];
    guardianCommitment: string;
    procedureThresholds: Array<{ procedure: ProcedureName; threshold: number }>;
  } | null;
  guardianState: SerializedAccountState | null;
  detectedConfig: SerializedDetectedMultisigConfig | null;
  proposals: SerializedProposal[];
  consumableNotes: SerializedConsumableNote[];
  lastError: string | null;
  busyAction: string | null;
}

export type SmokeEventOutcome = 'succeeded' | 'failed';

export interface SmokeEventEntry {
  id: number;
  timestamp: string;
  action: string;
  outcome: SmokeEventOutcome;
  error: string | null;
  durationMs: number;
}

export function serializeVaultBalance(balance: VaultBalance): SerializedVaultBalance {
  return {
    faucetId: balance.faucetId,
    amount: balance.amount.toString(),
  };
}

export function serializeDetectedMultisigConfig(
  config: DetectedMultisigConfig,
): SerializedDetectedMultisigConfig {
  return {
    threshold: config.threshold,
    numSigners: config.numSigners,
    signerCommitments: [...config.signerCommitments],
    guardianEnabled: config.guardianEnabled,
    guardianCommitment: config.guardianCommitment,
    vaultBalances: config.vaultBalances.map(serializeVaultBalance),
    procedureThresholds: [...config.procedureThresholds.entries()]
      .map(([procedure, threshold]) => ({ procedure, threshold }))
      .sort((left, right) => left.procedure.localeCompare(right.procedure)),
  };
}

export function serializeConsumableNote(note: ConsumableNote): SerializedConsumableNote {
  return {
    id: note.id,
    assets: note.assets.map((asset: ConsumableNote['assets'][number]) => ({
      faucetId: asset.faucetId,
      amount: asset.amount.toString(),
    })),
  };
}

export function serializeProposalSignature(
  signature: ProposalSignatureEntry,
): SerializedProposalSignatureEntry {
  return {
    signerId: signature.signerId,
    timestamp: signature.timestamp,
    signature: {
      scheme: signature.signature.scheme,
      signature: signature.signature.signature,
      ...(signature.signature.scheme === 'ecdsa' && signature.signature.publicKey
        ? { publicKey: signature.signature.publicKey }
        : {}),
    },
  };
}

export function serializeProposal(proposal: Proposal): SerializedProposal {
  return {
    id: proposal.id,
    accountId: proposal.accountId,
    nonce: proposal.nonce,
    status: proposal.status,
    txSummary: proposal.txSummary,
    signatures: proposal.signatures.map(serializeProposalSignature),
    metadata: proposal.metadata,
  };
}

export function serializeExternalWalletState(
  state: ExternalWalletState,
): SerializedExternalWalletState {
  return {
    source: state.source,
    connected: state.connected,
    publicKey: state.publicKey,
    commitment: state.commitment,
    scheme: state.scheme,
  };
}

export function serializeSignerInfo(signer: SignerInfo): SerializedSignerInfo {
  return {
    activeScheme: signer.activeScheme,
    falconCommitment: signer.falcon.commitment,
    ecdsaCommitment: signer.ecdsa.commitment,
  };
}
