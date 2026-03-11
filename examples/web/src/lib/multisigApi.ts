import {
  type Multisig,
  type MultisigClient,
  type Proposal,
  type AccountState,
  type MultisigConfig,
  type ConsumableNote,
  type ProcedureThreshold,
  MultisigClient as MultisigClientClass,
  FalconSigner,
  AccountInspector,
  type DetectedMultisigConfig,
} from '@openzeppelin/miden-multisig-client';
import type { WebClient } from '@miden-sdk/miden-sdk';
import type { SignerInfo } from '@/types';

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

/**
 * Initialize MultisigClient and get PSM pubkey.
 */
export async function initMultisigClient(
  webClient: WebClient,
  psmEndpoint: string,
  midenRpcEndpoint: string,
): Promise<{ client: MultisigClient; psmPubkey: string }> {
  const client = new MultisigClientClass(webClient, { psmEndpoint, midenRpcEndpoint });
  const psmPubkey = await client.psmClient.getPubkey();
  return { client, psmPubkey };
}

/**
 * Create a new multisig account.
 */
export async function createMultisigAccount(
  multisigClient: MultisigClient,
  signer: SignerInfo,
  otherCommitments: string[],
  threshold: number,
  psmCommitment: string,
  procedureThresholds?: ProcedureThreshold[],
): Promise<Multisig> {
  const signerCommitments = [signer.commitment, ...otherCommitments];
  const config: MultisigConfig = {
    threshold,
    signerCommitments,
    psmCommitment,
    psmEnabled: true,
    procedureThresholds,
    storageMode: 'private',
  };
  const falconSigner = new FalconSigner(signer.secretKey);
  return multisigClient.create(config, falconSigner);
}

/**
 * Load an existing multisig account from PSM.
 */
export async function loadMultisigAccount(
  multisigClient: MultisigClient,
  accountId: string,
  signer: SignerInfo,
): Promise<Multisig> {
  const falconSigner = new FalconSigner(signer.secretKey);
  return multisigClient.load(accountId, falconSigner);
}

/**
 * Register an account on PSM server.
 */
export async function registerOnPsm(multisig: Multisig): Promise<void> {
  await multisig.registerOnPsm();
}

/**
 * Register an account on PSM server using existing state data.
 * Used when switching PSM endpoints with an active multisig.
 */
export async function registerOnPsmWithState(
  multisig: Multisig,
  stateDataBase64: string,
): Promise<void> {
  await multisig.registerOnPsm(stateDataBase64);
}

/**
 * Switch an existing multisig to a new PSM endpoint.
 */
export async function switchMultisigPsm(
  multisigClient: MultisigClient,
  multisig: Multisig,
  stateDataBase64: string,
): Promise<void> {
  multisig.setPsmClient(multisigClient.psmClient);
  await multisig.registerOnPsm(stateDataBase64);
}

/**
 * Fetch account state from PSM and detect config.
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
  const newThreshold = increaseThreshold ? multisig.threshold + 1 : undefined;
  const proposal = await multisig.createAddSignerProposal(commitment, proposalNonce(multisig), newThreshold);
  const proposals = await syncVisibleProposals(multisig);
  // Ensure the new proposal is included
  if (!proposals.find((p) => p.id === proposal.id)) {
    return { proposal, proposals: filterVisibleProposals(multisig, [...proposals, proposal]) };
  }
  return { proposal, proposals };
}

/**
 * Create a "remove signer" proposal.
 */
export async function createRemoveSignerProposal(
  multisig: Multisig,
  signerToRemove: string,
  newThreshold?: number,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  const proposal = await multisig.createRemoveSignerProposal(
    signerToRemove,
    proposalNonce(multisig),
    newThreshold,
  );
  const proposals = await syncVisibleProposals(multisig);
  if (!proposals.find((p) => p.id === proposal.id)) {
    return { proposal, proposals: filterVisibleProposals(multisig, [...proposals, proposal]) };
  }
  return { proposal, proposals };
}

/**
 * Create a "change threshold" proposal.
 */
export async function createChangeThresholdProposal(
  multisig: Multisig,
  newThreshold: number,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  const proposal = await multisig.createChangeThresholdProposal(newThreshold, proposalNonce(multisig));
  const proposals = await syncVisibleProposals(multisig);
  if (!proposals.find((p) => p.id === proposal.id)) {
    return { proposal, proposals: filterVisibleProposals(multisig, [...proposals, proposal]) };
  }
  return { proposal, proposals };
}

export async function createUpdateProcedureThresholdProposal(
  multisig: Multisig,
  procedure: ProcedureName,
  threshold: number,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  const proposal = await multisig.createUpdateProcedureThresholdProposal(
    procedure,
    threshold,
    proposalNonce(multisig),
  );
  const proposals = await syncVisibleProposals(multisig);
  if (!proposals.find((p) => p.id === proposal.id)) {
    return { proposal, proposals: filterVisibleProposals(multisig, [...proposals, proposal]) };
  }
  return { proposal, proposals };
}

/**
 * Create a "consume notes" proposal.
 */
export async function createConsumeNotesProposal(
  multisig: Multisig,
  noteIds: string[],
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  const proposal = await multisig.createConsumeNotesProposal(noteIds, proposalNonce(multisig));
  const proposals = await syncVisibleProposals(multisig);
  if (!proposals.find((p) => p.id === proposal.id)) {
    return { proposal, proposals: filterVisibleProposals(multisig, [...proposals, proposal]) };
  }
  return { proposal, proposals };
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
  const proposal = await multisig.createP2idProposal(
    recipientId,
    faucetId,
    amount,
    proposalNonce(multisig),
  );
  const proposals = await syncVisibleProposals(multisig);
  if (!proposals.find((p) => p.id === proposal.id)) {
    return { proposal, proposals: filterVisibleProposals(multisig, [...proposals, proposal]) };
  }
  return { proposal, proposals };
}

/**
 * Create a "switch PSM" proposal.
 * This is stored locally only (no PSM sync) since the current PSM may be unavailable.
 */
export async function createSwitchPsmProposal(
  multisig: Multisig,
  newPsmEndpoint: string,
  newPsmPubkey: string,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  const proposal = await multisig.createSwitchPsmProposal(
    newPsmEndpoint,
    newPsmPubkey,
    proposalNonce(multisig),
  );
  const proposals = listVisibleProposals(multisig);
  return { proposal, proposals };
}

/**
 * Sign a proposal online (submits to PSM).
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
export function signProposalOffline(
  multisig: Multisig,
  proposalId: string,
): { json: string; proposals: Proposal[] } {
  const json = multisig.signProposalOffline(proposalId);
  const proposals = listVisibleProposals(multisig);
  return { json, proposals };
}

/**
 * Import a proposal from JSON.
 */
export function importProposal(
  multisig: Multisig,
  json: string,
): { proposal: Proposal; proposals: Proposal[] } {
  const proposal = multisig.importProposal(json);
  const proposals = listVisibleProposals(multisig);
  return { proposal, proposals };
}
