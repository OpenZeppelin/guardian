import {
  type Multisig,
  type MultisigClient,
  type Proposal,
  type AccountState,
  type MultisigConfig,
  type ConsumableNote,
  type ProcedureThreshold,
  type ProcedureName,
  type SignatureScheme,
  type Signer,
  MultisigClient as MultisigClientClass,
  FalconSigner,
  EcdsaSigner,
  ParaSigner,
  MidenWalletSigner,
  type WalletSigningContext,
  AccountInspector,
  type DetectedMultisigConfig,
} from '@openzeppelin/miden-multisig-client';
import type { WebClient } from '@miden-sdk/miden-sdk';
import type { SignerInfo } from '@/types';
import type { WalletSource } from '@/wallets/types';

type ResolvedSigner = {
  commitment: string;
  signatureScheme: SignatureScheme;
  signerInstance: Signer;
  walletSource: WalletSource;
};

interface ParaSignerOptions {
  paraClient: { signMessage(params: { walletId: string; messageBase64: string }): Promise<unknown> };
  walletId: string;
  commitment: string;
  publicKey: string;
}

interface MidenWalletSignerOptions {
  wallet: WalletSigningContext;
  commitment: string;
  publicKey: string;
  scheme: SignatureScheme;
}

export function resolveLocalSigner(
  signer: SignerInfo,
  signatureScheme: SignatureScheme = signer.activeScheme,
): ResolvedSigner {
  if (signatureScheme === 'ecdsa') {
    return {
      commitment: signer.ecdsa.commitment,
      signatureScheme,
      signerInstance: new EcdsaSigner(signer.ecdsa.secretKey),
      walletSource: 'local',
    };
  }

  return {
    commitment: signer.falcon.commitment,
    signatureScheme,
    signerInstance: new FalconSigner(signer.falcon.secretKey),
    walletSource: 'local',
  };
}

export function resolveParaSigner({
  paraClient,
  walletId,
  commitment,
  publicKey,
}: ParaSignerOptions): ResolvedSigner {
  return {
    commitment,
    signatureScheme: 'ecdsa',
    signerInstance: new ParaSigner(paraClient, walletId, commitment, publicKey),
    walletSource: 'para',
  };
}

export function resolveMidenWalletSigner({
  wallet,
  commitment,
  publicKey,
  scheme,
}: MidenWalletSignerOptions): ResolvedSigner {
  return {
    commitment,
    signatureScheme: scheme,
    signerInstance: new MidenWalletSigner(wallet, commitment, scheme, undefined, publicKey),
    walletSource: 'miden-wallet',
  };
}

function currentAccountNonce(multisig: Multisig): number | null {
  if (!multisig.account) {
    return null;
  }

  try {
    const nonce = multisig.account.nonce().asInt();
    if (nonce > BigInt(Number.MAX_SAFE_INTEGER)) {
      return null;
    }
    return Number(nonce);
  } catch {
    return null;
  }
}

function proposalNonce(multisig: Multisig): number | undefined {
  const nonce = currentAccountNonce(multisig);
  return nonce === null ? undefined : nonce;
}

function filterVisibleProposals(
  multisig: Multisig,
  proposals: Proposal[],
  state?: AccountState,
): Proposal[] {
  const accountNonce = currentAccountNonce(multisig);
  const stateUpdatedAtMs = state ? Date.parse(state.updatedAt) : Number.NaN;

  return proposals.filter((proposal) => {
    if (proposal.status === 'finalized') {
      return false;
    }

    if (accountNonce !== null && proposal.nonce < accountNonce) {
      return false;
    }

    const hasTimestampStyleNonce = proposal.nonce >= 1_000_000_000_000;
    if (
      hasTimestampStyleNonce &&
      Number.isFinite(stateUpdatedAtMs) &&
      proposal.nonce < stateUpdatedAtMs
    ) {
      return false;
    }

    return true;
  });
}

async function syncVisibleProposals(multisig: Multisig): Promise<Proposal[]> {
  const proposals = await multisig.syncProposals();
  return filterVisibleProposals(multisig, proposals);
}

function listVisibleProposals(multisig: Multisig): Proposal[] {
  return filterVisibleProposals(multisig, multisig.listProposals());
}

async function createProposalResult(
  multisig: Multisig,
  createProposal: () => Promise<Proposal>,
  loadProposals: (multisig: Multisig) => Promise<Proposal[]> = syncVisibleProposals,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  const proposal = await createProposal();
  const proposals = await loadProposals(multisig);

  if (proposals.some((candidate) => candidate.id === proposal.id)) {
    return { proposal, proposals };
  }

  return {
    proposal,
    proposals: filterVisibleProposals(multisig, [...proposals, proposal]),
  };
}

/**
 * Initialize MultisigClient and get GUARDIAN pubkey.
 */
export async function initMultisigClient(
  webClient: WebClient,
  guardianEndpoint: string,
  midenRpcEndpoint: string,
): Promise<{ client: MultisigClient; guardianPubkey: string }> {
  const client = new MultisigClientClass(webClient, { guardianEndpoint, midenRpcEndpoint });
  const response = await client.guardianClient.getPubkey();
  const guardianPubkey = typeof response === 'string' ? response : response.commitment;
  return { client, guardianPubkey };
}

/**
 * Create a new multisig account.
 */
export async function createMultisigAccount(
  multisigClient: MultisigClient,
  signer: ResolvedSigner,
  otherCommitments: string[],
  threshold: number,
  guardianCommitment: string,
  procedureThresholds?: ProcedureThreshold[],
  signatureScheme: SignatureScheme = signer.signatureScheme,
): Promise<Multisig> {
  const signerCommitments = [signer.commitment, ...otherCommitments];
  const config: MultisigConfig = {
    threshold,
    signerCommitments,
    guardianCommitment,
    guardianEnabled: true,
    procedureThresholds,
    storageMode: 'private',
    signatureScheme,
  };
  return multisigClient.create(config, signer.signerInstance);
}

/**
 * Load an existing multisig account from GUARDIAN.
 */
export async function loadMultisigAccount(
  multisigClient: MultisigClient,
  accountId: string,
  signer: ResolvedSigner,
): Promise<Multisig> {
  return multisigClient.load(accountId, signer.signerInstance);
}

/**
 * Register an account on GUARDIAN server.
 */
export async function registerOnGuardian(multisig: Multisig): Promise<void> {
  await multisig.registerOnGuardian();
}

/**
 * Register an account on GUARDIAN server using existing state data.
 * Used when switching GUARDIAN endpoints with an active multisig.
 */
export async function registerOnGuardianWithState(
  multisig: Multisig,
  stateDataBase64: string,
): Promise<void> {
  await multisig.registerOnGuardian(stateDataBase64);
}

/**
 * Switch an existing multisig to a new GUARDIAN endpoint.
 */
export async function switchMultisigGuardian(
  multisigClient: MultisigClient,
  multisig: Multisig,
  stateDataBase64: string,
): Promise<void> {
  multisig.setGuardianClient(multisigClient.guardianClient);
  await multisig.registerOnGuardian(stateDataBase64);
}

/**
 * Fetch account state from GUARDIAN and detect config.
 */
export async function fetchAccountState(
  multisig: Multisig,
): Promise<{ state: AccountState; config: DetectedMultisigConfig }> {
  const state = await multisig.syncState();
  const config = AccountInspector.fromBase64(state.stateDataBase64);
  return { state, config };
}

/**
 * Sync proposals, state, and consumable notes.
 */
export async function syncAll(
  multisig: Multisig,
): Promise<{ proposals: Proposal[]; state: AccountState; notes: ConsumableNote[] }> {
  const state = await multisig.syncState();
  const proposals = filterVisibleProposals(multisig, await multisig.syncProposals(), state);
  const notes = await multisig.getConsumableNotes();
  return { proposals, state, notes };
}

export async function verifyStateCommitment(
  multisig: Multisig,
): Promise<{
  accountId: string;
  localCommitment: string;
  onChainCommitment: string;
}> {
  return multisig.verifyStateCommitment();
}

/**
 * Create an "add signer" proposal.
 */
export async function createAddSignerProposal(
  multisig: Multisig,
  commitment: string,
  increaseThreshold: boolean,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  return createProposalResult(multisig, () => {
    const newThreshold = increaseThreshold ? multisig.threshold + 1 : undefined;
    return multisig.createAddSignerProposal(commitment, proposalNonce(multisig), newThreshold);
  });
}

/**
 * Create a "remove signer" proposal.
 */
export async function createRemoveSignerProposal(
  multisig: Multisig,
  signerToRemove: string,
  newThreshold?: number,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  return createProposalResult(multisig, () =>
    multisig.createRemoveSignerProposal(
      signerToRemove,
      proposalNonce(multisig),
      newThreshold,
    ));
}

/**
 * Create a "change threshold" proposal.
 */
export async function createChangeThresholdProposal(
  multisig: Multisig,
  newThreshold: number,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  return createProposalResult(multisig, () =>
    multisig.createChangeThresholdProposal(newThreshold, proposalNonce(multisig)));
}

export async function createUpdateProcedureThresholdProposal(
  multisig: Multisig,
  procedure: ProcedureName,
  threshold: number,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  return createProposalResult(multisig, () =>
    multisig.createUpdateProcedureThresholdProposal(
      procedure,
      threshold,
      proposalNonce(multisig),
    ));
}

/**
 * Create a "consume notes" proposal.
 */
export async function createConsumeNotesProposal(
  multisig: Multisig,
  noteIds: string[],
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  return createProposalResult(multisig, () =>
    multisig.createConsumeNotesProposal(noteIds, proposalNonce(multisig)));
}

/**
 * Create a P2ID (send payment) proposal.
 */
export async function createP2idProposal(
  multisig: Multisig,
  recipientId: string,
  faucetId: string,
  amount: bigint,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  return createProposalResult(multisig, () =>
    multisig.createP2idProposal(
      recipientId,
      faucetId,
      amount,
      proposalNonce(multisig),
    ));
}

/**
 * Create a "switch GUARDIAN" proposal.
 * This is stored locally only (no GUARDIAN sync) since the current GUARDIAN may be unavailable.
 */
export async function createSwitchGuardianProposal(
  multisig: Multisig,
  newGuardianEndpoint: string,
  newGuardianPubkey: string,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  return createProposalResult(
    multisig,
    () =>
      multisig.createSwitchGuardianProposal(
        newGuardianEndpoint,
        newGuardianPubkey,
        proposalNonce(multisig),
      ),
    async (currentMultisig) => listVisibleProposals(currentMultisig),
  );
}

/**
 * Sign a proposal online (submits to GUARDIAN).
 */
export async function signProposal(
  multisig: Multisig,
  proposalId: string,
): Promise<Proposal[]> {
  await multisig.signProposal(proposalId);
  return syncVisibleProposals(multisig);
}

/**
 * Execute a proposal that has enough signatures.
 */
export async function executeProposal(
  multisig: Multisig,
  proposalId: string,
): Promise<void> {
  await multisig.executeProposal(proposalId);
}

/**
 * Export a proposal to JSON for offline sharing.
 */
export function exportProposalToJson(
  multisig: Multisig,
  proposalId: string,
): string {
  return multisig.exportProposalToJson(proposalId);
}

/**
 * Sign a proposal offline and return the signed JSON.
 */
export async function signProposalOffline(
  multisig: Multisig,
  proposalId: string,
): Promise<{ json: string; proposals: Proposal[] }> {
  const json = await multisig.signProposalOffline(proposalId);
  const proposals = listVisibleProposals(multisig);
  return { json, proposals };
}

/**
 * Import a proposal from JSON.
 */
export async function importProposal(
  multisig: Multisig,
  json: string,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  const proposal = await multisig.importProposal(json);
  const proposals = listVisibleProposals(multisig);
  return { proposal, proposals };
}
