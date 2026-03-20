import type { WebClient } from '@miden-sdk/miden-sdk';
import {
  AccountInspector,
  EcdsaSigner,
  FalconSigner,
  MidenWalletSigner,
  MultisigClient as MultisigClientClass,
  ParaSigner,
  type AccountState,
  type ConsumableNote,
  type DetectedMultisigConfig,
  type Multisig,
  type MultisigClient,
  type MultisigConfig,
  type ProcedureName,
  type ProcedureThreshold,
  type Proposal,
  type SignatureScheme,
  type WalletSigningContext,
} from '@openzeppelin/miden-multisig-client';
import type { SignerInfo, ResolvedSigner } from './types';

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

export function filterVisibleProposals(
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

export async function syncVisibleProposals(multisig: Multisig): Promise<Proposal[]> {
  const proposals = await multisig.syncProposals();
  return filterVisibleProposals(multisig, proposals);
}

export function listVisibleProposals(multisig: Multisig): Proposal[] {
  return filterVisibleProposals(multisig, multisig.listProposals());
}

async function createProposalResult(
  multisig: Multisig,
  createProposal: () => Promise<Proposal>,
  loadProposals: (target: Multisig) => Promise<Proposal[]> = syncVisibleProposals,
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

export async function loadMultisigAccount(
  multisigClient: MultisigClient,
  accountId: string,
  signer: ResolvedSigner,
): Promise<Multisig> {
  return multisigClient.load(accountId, signer.signerInstance);
}

export async function registerOnGuardian(multisig: Multisig): Promise<void> {
  await multisig.registerOnGuardian();
}

export async function registerOnGuardianWithState(
  multisig: Multisig,
  stateDataBase64: string,
): Promise<void> {
  await multisig.registerOnGuardian(stateDataBase64);
}

export async function switchMultisigGuardian(
  multisigClient: MultisigClient,
  multisig: Multisig,
  stateDataBase64: string,
): Promise<void> {
  multisig.setGuardianClient(multisigClient.guardianClient);
  await multisig.registerOnGuardian(stateDataBase64);
}

export async function fetchAccountState(
  multisig: Multisig,
): Promise<{ state: AccountState; config: DetectedMultisigConfig }> {
  const state = await multisig.syncState();
  const config = AccountInspector.fromBase64(state.stateDataBase64);
  return { state, config };
}

export async function syncAll(
  multisig: Multisig,
): Promise<{ proposals: Proposal[]; state: AccountState; notes: ConsumableNote[] }> {
  const state = await multisig.syncState();
  const proposals = filterVisibleProposals(multisig, await multisig.syncProposals(), state);
  const notes = await multisig.getConsumableNotes();
  return { proposals, state, notes };
}

export async function verifyStateCommitment(multisig: Multisig): Promise<{
  accountId: string;
  localCommitment: string;
  onChainCommitment: string;
}> {
  return multisig.verifyStateCommitment();
}

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

export async function createConsumeNotesProposal(
  multisig: Multisig,
  noteIds: string[],
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  return createProposalResult(multisig, () =>
    multisig.createConsumeNotesProposal(noteIds, proposalNonce(multisig)));
}

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

export async function signProposal(
  multisig: Multisig,
  proposalId: string,
): Promise<Proposal[]> {
  await multisig.signProposal(proposalId);
  return syncVisibleProposals(multisig);
}

export async function executeProposal(
  multisig: Multisig,
  proposalId: string,
): Promise<void> {
  await multisig.executeProposal(proposalId);
}

export function exportProposalToJson(
  multisig: Multisig,
  proposalId: string,
): string {
  return multisig.exportProposalToJson(proposalId);
}

export async function signProposalOffline(
  multisig: Multisig,
  proposalId: string,
): Promise<{ json: string; proposals: Proposal[] }> {
  const json = await multisig.signProposalOffline(proposalId);
  const proposals = listVisibleProposals(multisig);
  return { json, proposals };
}

export async function importProposal(
  multisig: Multisig,
  json: string,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  const proposal = await multisig.importProposal(json);
  const proposals = listVisibleProposals(multisig);
  return { proposal, proposals };
}
