import {
  type Multisig,
  type MultisigClient,
  type Proposal,
  type AccountState,
  type MultisigConfig,
  type ConsumableNote,
  MultisigClient as MultisigClientClass,
  FalconSigner,
  AccountInspector,
  type DetectedMultisigConfig,
} from '@openzeppelin/miden-multisig-client';
import type { WebClient } from '@demox-labs/miden-sdk';
import type { SignerInfo } from '@/types';

/**
 * Initialize MultisigClient and get PSM pubkey.
 */
export async function initMultisigClient(
  webClient: WebClient,
  psmEndpoint: string,
): Promise<{ client: MultisigClient; psmPubkey: string }> {
  const client = new MultisigClientClass(webClient, { psmEndpoint });
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
): Promise<Multisig> {
  const signerCommitments = [signer.commitment, ...otherCommitments];
  const config: MultisigConfig = {
    threshold,
    signerCommitments,
    psmCommitment,
    psmEnabled: true,
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
 * Fetch account state from PSM and detect config.
 */
export async function fetchAccountState(
  multisig: Multisig,
): Promise<{ state: AccountState; config: DetectedMultisigConfig }> {
  const state = await multisig.fetchState();
  const config = AccountInspector.fromBase64(state.stateDataBase64);
  return { state, config };
}

/**
 * Sync proposals, state, and consumable notes.
 */
export async function syncAll(
  multisig: Multisig,
): Promise<{ proposals: Proposal[]; state: AccountState; notes: ConsumableNote[] }> {
  const proposals = await multisig.syncProposals();
  const state = await multisig.fetchState();
  const notes = await multisig.getConsumableNotes();
  return { proposals, state, notes };
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
  const proposal = await multisig.createAddSignerProposal(commitment, undefined, newThreshold);
  const proposals = await multisig.syncProposals();
  // Ensure the new proposal is included
  if (!proposals.find((p) => p.id === proposal.id)) {
    return { proposal, proposals: [...proposals, proposal] };
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
  const proposal = await multisig.createRemoveSignerProposal(signerToRemove, undefined, newThreshold);
  const proposals = await multisig.syncProposals();
  if (!proposals.find((p) => p.id === proposal.id)) {
    return { proposal, proposals: [...proposals, proposal] };
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
  const proposal = await multisig.createChangeThresholdProposal(newThreshold);
  const proposals = await multisig.syncProposals();
  if (!proposals.find((p) => p.id === proposal.id)) {
    return { proposal, proposals: [...proposals, proposal] };
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
  const proposal = await multisig.createConsumeNotesProposal(noteIds);
  const proposals = await multisig.syncProposals();
  if (!proposals.find((p) => p.id === proposal.id)) {
    return { proposal, proposals: [...proposals, proposal] };
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
  const proposal = await multisig.createP2idProposal(recipientId, faucetId, amount);
  const proposals = await multisig.syncProposals();
  if (!proposals.find((p) => p.id === proposal.id)) {
    return { proposal, proposals: [...proposals, proposal] };
  }
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
  return multisig.syncProposals();
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
  const proposals = multisig.listProposals();
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
  const proposals = multisig.listProposals();
  return { proposal, proposals };
}
