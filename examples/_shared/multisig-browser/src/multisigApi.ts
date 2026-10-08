import {
  MidenClient,
  Word,
  type AdviceMap,
  type NoteType,
  type TransactionRequest,
} from '@miden-sdk/miden-sdk';
import {
  AccountInspector,
  buildP2idTransactionRequest,
  chainAnchorBlockNum,
  EcdsaSigner,
  FalconSigner,
  MidenWalletSigner,
  MultisigClient as MultisigClientClass,
  type AccountState,
  type ConsumableNote,
  type DetectedMultisigConfig,
  type ExecutionFailure,
  type Multisig,
  type MultisigClient,
  type MultisigConfig,
  type ProcedureName,
  type ProcedureThreshold,
  type Proposal,
  type ProposalExecution,
  type ProposalExecutionMode,
  type SignatureScheme,
  type WalletSigningContext,
} from '@openzeppelin/miden-multisig-client';
import type { SignerInfo, ResolvedSigner } from './types';

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

    // `<=` matches the next-nonce convention: a proposal at nonce N is consumed
    // once the account reaches nonce N.
    if (accountNonce !== null && proposal.nonce <= accountNonce) {
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
  midenClient: MidenClient,
  guardianEndpoint: string,
  midenRpcEndpoint: string,
  prover?: import('@openzeppelin/miden-multisig-client').ProverConfig,
  rpc?: import('@openzeppelin/miden-multisig-client').RpcConfig,
  executionMode: ProposalExecutionMode = 'self_executed',
): Promise<{ client: MultisigClient; guardianPubkey: string }> {
  const client = new MultisigClientClass(midenClient, {
    guardianEndpoint,
    midenRpcEndpoint,
    prover,
    rpc,
    executionMode,
  });
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
  // An explicit fetch of GUARDIAN's copy for display and config detection;
  // store reconciliation (with the canonical-nonce pre-check) is `syncAll`.
  const state = await multisig.fetchState();
  const config = AccountInspector.fromBase64(state.stateDataBase64);
  return { state, config };
}

export async function syncAll(
  multisig: Multisig,
  lastFetchedState?: AccountState,
): Promise<{ proposals: Proposal[]; state: AccountState | null; notes: ConsumableNote[] }> {
  // `state` is null when GUARDIAN reported nothing newer than the local
  // account (the canonical-nonce pre-check skipped the state fetch); the
  // caller's last fetched copy then keeps the proposal filter's inputs stable.
  const synced = await multisig.syncState();
  const state = synced.source === 'guardian' ? synced.state : null;
  const proposals = filterVisibleProposals(
    multisig,
    await multisig.syncProposals(),
    state ?? lastFetchedState,
  );
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
    return multisig.createAddSignerProposal(commitment, {
      newThreshold,
    });
  });
}

export async function createRemoveSignerProposal(
  multisig: Multisig,
  signerToRemove: string,
  newThreshold?: number,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  return createProposalResult(multisig, () =>
    multisig.createRemoveSignerProposal(signerToRemove, {
      newThreshold,
    }));
}

export async function createChangeThresholdProposal(
  multisig: Multisig,
  newThreshold: number,
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  return createProposalResult(multisig, () =>
    multisig.createChangeThresholdProposal(newThreshold));
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
    ));
}

export async function createConsumeNotesProposal(
  multisig: Multisig,
  noteIds: string[],
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  return createProposalResult(multisig, () =>
    multisig.createConsumeNotesProposal(noteIds));
}

export async function createP2idProposal(
  multisig: Multisig,
  recipientId: string,
  faucetId: string,
  amount: bigint,
  noteType?: NoteType,
  heights?: { reclaimHeight?: number; timelockHeight?: number },
): Promise<{ proposal: Proposal; proposals: Proposal[] }> {
  return createProposalResult(multisig, () =>
    multisig.createP2idProposal(recipientId, faucetId, amount, {
      ...heights,
      noteType,
    }));
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

/**
 * Hands a threshold-met proposal to GUARDIAN, which proves and submits it, and waits until the
 * execution is `committed` or `failed`. The proposal must have been created by a client in
 * `guardian_executable` mode.
 */
export async function executeThroughGuardian(
  multisig: Multisig,
  proposalId: string,
): Promise<ProposalExecution> {
  await multisig.requestGuardianExecution(proposalId);
  return multisig.waitForGuardianExecution(proposalId);
}

/** The account's in-flight GUARDIAN execution and, when asked, one proposal's latest. */
export async function guardianExecutionStatus(
  multisig: Multisig,
  proposalId?: string,
): Promise<{ current: ProposalExecution | null; proposal: ProposalExecution | null }> {
  const current = await multisig.currentExecution();
  const proposal = proposalId ? await multisig.executionStatus(proposalId) : null;
  return { current, proposal };
}

/**
 * What the caller can do after a failed execution. Follows the failure code: `proposalExists`
 * says whether the proposal is still stored, not whether executing it again can succeed.
 */
function failedExecutionAdvice(error: ExecutionFailure): string {
  switch (error.code) {
    case 'GUARDIAN_EXECUTION_CHAIN_BEHIND':
    case 'GUARDIAN_EXECUTION_NODE_UNAVAILABLE':
    case 'GUARDIAN_EXECUTION_CHAIN_INCONSISTENT':
    case 'GUARDIAN_EXECUTION_PROVING_FAILED':
    case 'GUARDIAN_EXECUTION_SEALING_FAILED':
    case 'GUARDIAN_EXECUTION_ACKNOWLEDGEMENT_FAILED':
    case 'GUARDIAN_EXECUTION_LEASE_EXPIRED':
    case 'GUARDIAN_EXECUTION_ABANDONED':
      return 'Execute it again.';
    case 'GUARDIAN_EXECUTION_INSUFFICIENT_SIGNATURES':
      return 'Collect more signatures, then execute it again.';
    case 'GUARDIAN_EXECUTION_EXPIRATION_REACHED':
      return error.bound === 'transaction'
        ? 'Execute it again: a new attempt gets a fresh transaction window.'
        : 'Its approval window has passed: create and sign a new proposal.';
    case 'GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE':
      return error.reason === 'unavailable'
        ? 'Execute it again once the node serves the foreign account.'
        : 'GUARDIAN cannot read a private foreign account: execute it from a client that holds it.';
    case 'GUARDIAN_EXECUTION_BINDING_MISMATCH':
    case 'GUARDIAN_EXECUTION_STATE_MISMATCH':
    case 'GUARDIAN_EXECUTION_REQUEST_CODEC':
    case 'GUARDIAN_EXECUTION_PROTOCOL_MISMATCH':
    case 'GUARDIAN_EXECUTION_INSUFFICIENT_FEE':
    case 'GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON':
    case 'GUARDIAN_EXECUTION_ACCOUNT_INADMISSIBLE':
      return 'Fix the cause before executing it again.';
    case 'GUARDIAN_EXECUTION_REQUEST_INVALID':
      return 'GUARDIAN cannot execute this request: execute it from this client instead.';
    case 'GUARDIAN_EXECUTION_SUBMISSION_REJECTED':
    case 'GUARDIAN_EXECUTION_CANDIDATE_DISCARDED':
    case 'GUARDIAN_EXECUTION_EXPIRED':
      return 'The transaction was sent and did not land: create and sign a new proposal.';
    default: {
      const unreachable: never = error;
      throw new Error(`Unknown execution failure: ${JSON.stringify(unreachable)}`);
    }
  }
}

/** One line naming why GUARDIAN did not commit an execution and what the caller can do next. */
export function describeFailedExecution(execution: ProposalExecution): string {
  if (!execution.error) {
    return 'GUARDIAN could not execute the proposal and reported no cause.';
  }
  const stored = execution.proposalExists ? 'still stored' : 'removed';
  return (
    `GUARDIAN could not execute the proposal: ${execution.error.message} (${execution.error.code}). ` +
    `The proposal is ${stored}. ${failedExecutionAdvice(execution.error)}`
  );
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

export interface CustomProposalRecipe {
  proposalId: string;
  label: string;
  senderId: string;
  recipientId: string;
  faucetId: string;
  amount: string;
  saltHex: string;
  /** The block the signed summary binds: the proposal's anchor block. */
  boundBlockNum: number;
}

/**
 * The integration's own recipe rebuilds the exact request at execute time. The
 * client attaches the multisig auth args, so the recipe pins everything they
 * bind: the salt and the block, both of which the cosigners signed over.
 */
async function buildRequestFromRecipe(
  midenClient: MidenClient,
  recipe: CustomProposalRecipe,
  signatureAdviceMap?: AdviceMap,
): Promise<TransactionRequest> {
  const { request } = await buildP2idTransactionRequest(
    midenClient,
    recipe.senderId,
    recipe.recipientId,
    recipe.faucetId,
    BigInt(recipe.amount),
    {
      salt: Word.fromHex(recipe.saltHex),
      boundBlockNum: recipe.boundBlockNum,
      signatureAdviceMap,
    },
  );
  return request;
}

function proposalBoundBlockNum(proposal: Proposal): number {
  if (!proposal.metadata.chainAnchor) {
    throw new Error(`Proposal ${proposal.id} carries no chain anchor`);
  }
  return chainAnchorBlockNum(proposal.metadata.chainAnchor);
}

export async function createCustomP2idProposal(
  midenClient: MidenClient,
  multisig: Multisig,
  recipientId: string,
  faucetId: string,
  amount: bigint,
  label: string,
): Promise<{ proposal: Proposal; proposals: Proposal[]; recipe: CustomProposalRecipe }> {
  const senderId = multisig.accountId;
  const { request, salt } = await buildP2idTransactionRequest(
    midenClient,
    senderId,
    recipientId,
    faucetId,
    amount,
  );

  const created = await createProposalResult(multisig, () =>
    multisig.createCustomProposal(request.serialize(), label));

  const recipe: CustomProposalRecipe = {
    proposalId: created.proposal.id,
    label,
    senderId,
    recipientId,
    faucetId,
    amount: amount.toString(),
    saltHex: salt.toHex(),
    boundBlockNum: proposalBoundBlockNum(created.proposal),
  };

  return { ...created, recipe };
}

export async function prepareAndSubmitCustomProposal(
  midenClient: MidenClient,
  multisig: Multisig,
  recipe: CustomProposalRecipe,
): Promise<void> {
  const bindingRequest = await buildRequestFromRecipe(midenClient, recipe);
  const advice = await multisig.prepareCustomExecution(recipe.proposalId, bindingRequest.serialize());

  const finalRequest = await buildRequestFromRecipe(midenClient, recipe, advice);

  try {
    await multisig.submitTransaction(recipe.proposalId, finalRequest);
  } catch (submitError) {
    // The local apply step can transiently fail (autoSync race) even when the
    // on-chain submit succeeded. Re-sync so local state catches up, then surface
    // the error — a generic nonce bump is not proof THIS submit landed (another
    // session could advance the account), so the operator should verify via the
    // refreshed state rather than have a false success swallowed here.
    await multisig.syncState();
    throw submitError;
  }
}
