import { type DeltaObject, type PsmHttpClient } from '@openzeppelin/psm-client';
import type { WebClient, TransactionRequest } from '@miden-sdk/miden-sdk';
import {
  AccountId,
  AdviceMap,
  FeltArray,
  Signature,
  TransactionSummary,
  Word,
} from '@miden-sdk/miden-sdk';
import type { ProposalType, TransactionProposal } from '../../types.js';
import {
  buildConsumeNotesTransactionRequest,
  buildP2idTransactionRequest,
  buildUpdatePsmTransactionRequest,
  buildUpdateProcedureThresholdTransactionRequest,
  buildUpdateSignersTransactionRequest,
} from '../../transaction.js';
import { base64ToUint8Array, normalizeHexWord } from '../../utils/encoding.js';
import {
  buildSignatureAdviceEntry,
  signatureHexToBytes,
  tryComputeEcdsaCommitmentHex,
} from '../../utils/signature.js';
import { computeCommitmentFromTxSummary } from '../helpers.js';

interface ResolveExecutionSourceResult {
  delta?: DeltaObject;
  txSummaryBase64: string;
}

interface PrepareExecutionResult {
  txSummary: TransactionSummary;
  saltHex: string;
  txCommitmentHex: string;
}

interface ExecuteProposalWorkflowParams {
  proposal: TransactionProposal;
  accountId: string;
  threshold: number;
  signerCommitments: string[];
  psmCommitment: string;
  psmPublicKey?: string;
  signatureScheme: 'falcon' | 'ecdsa';
  getEffectiveThreshold: (proposalType: ProposalType) => number;
  psm: PsmHttpClient;
  webClient: WebClient;
}

export async function executeProposalWorkflow(
  params: ExecuteProposalWorkflowParams,
): Promise<void> {
  ensureProposalReady(
    params.proposal,
    params.threshold,
    params.getEffectiveThreshold,
  );

  const isSwitchPsm = params.proposal.metadata.proposalType === 'switch_psm';
  const executionSource = await resolveExecutionSource(
    params.psm,
    params.accountId,
    params.proposal,
    isSwitchPsm,
  );
  const executionData = prepareExecutionData(executionSource.txSummaryBase64);

  const adviceMap = buildCosignerAdviceMap(
    params.proposal,
    params.signerCommitments,
    executionData.txCommitmentHex,
  );

  if (!isSwitchPsm && executionSource.delta) {
    await appendPsmAckAdvice(
      params.psm,
      executionSource.delta,
      params.psmCommitment,
      params.psmPublicKey,
      params.signatureScheme,
      executionData.txCommitmentHex,
      adviceMap,
    );
  }

  const finalRequest = await buildFinalRequest(
    params,
    executionData.saltHex,
    adviceMap,
  );
  await submitTransaction(params.webClient, params.accountId, finalRequest);
}


export async function createTransactionProposalRequest(
  params: ExecuteProposalWorkflowParams,
): Promise<TransactionRequest> {
  ensureProposalReady(
    params.proposal,
    params.threshold,
    params.getEffectiveThreshold,
  );

  const isSwitchPsm = params.proposal.metadata.proposalType === 'switch_psm';
  const executionSource = await resolveExecutionSource(
    params.psm,
    params.accountId,
    params.proposal,
    isSwitchPsm,
  );
  const executionData = prepareExecutionData(executionSource.txSummaryBase64);

  const adviceMap = buildCosignerAdviceMap(
    params.proposal,
    params.signerCommitments,
    executionData.txCommitmentHex,
  );

  if (!isSwitchPsm && executionSource.delta) {
    await appendPsmAckAdvice(
      params.psm,
      executionSource.delta,
      params.psmCommitment,
      params.psmPublicKey,
      params.signatureScheme,
      executionData.txCommitmentHex,
      adviceMap,
    );
  }

  return await buildFinalRequest(
    params,
    executionData.saltHex,
    adviceMap,
  );
}

function ensureProposalReady(
  proposal: TransactionProposal,
  defaultThreshold: number,
  getEffectiveThreshold: (proposalType: ProposalType) => number,
): void {
  const proposalType = proposal.metadata?.proposalType;
  const effectiveThreshold = proposalType
    ? getEffectiveThreshold(proposalType)
    : defaultThreshold;

  if (proposal.signatures.length < effectiveThreshold) {
    throw new Error('Proposal is not ready for execution. Still pending signatures.');
  }
}

async function resolveExecutionSource(
  psm: PsmHttpClient,
  accountId: string,
  proposal: TransactionProposal,
  isSwitchPsm: boolean,
): Promise<ResolveExecutionSourceResult> {
  if (isSwitchPsm) {
    return { txSummaryBase64: proposal.txSummary };
  }

  const deltas = await psm.getDeltaProposals(accountId);
  const delta = deltas.find(
    (d) => computeCommitmentFromTxSummary(d.deltaPayload.txSummary.data) === proposal.commitment,
  );

  if (!delta) {
    throw new Error(`Proposal not found on server: ${proposal.commitment}`);
  }

  return {
    delta,
    txSummaryBase64: delta.deltaPayload.txSummary.data,
  };
}

function prepareExecutionData(txSummaryBase64: string): PrepareExecutionResult {
  const txSummaryBytes = base64ToUint8Array(txSummaryBase64);
  const txSummary = TransactionSummary.deserialize(txSummaryBytes);
  const saltHex = txSummary.salt().toHex();
  const txCommitmentHex = txSummary.toCommitment().toHex();
  return { txSummary, saltHex, txCommitmentHex };
}

function buildCosignerAdviceMap(
  proposal: TransactionProposal,
  signerCommitments: string[],
  txCommitmentHex: string,
): AdviceMap {
  const adviceMap = new AdviceMap();
  const normalizedSignerCommitments = new Set(
    signerCommitments.map((c) => normalizeHexWord(c)),
  );

  for (const cosignerSig of proposal.signatures) {
    let signerCommitmentHex = normalizeHexWord(cosignerSig.signerId);
    if (cosignerSig.signature.scheme === 'ecdsa' && cosignerSig.signature.publicKey) {
      const derived = tryComputeEcdsaCommitmentHex(cosignerSig.signature.publicKey);
      if (derived && derived !== signerCommitmentHex) {
        if (!normalizedSignerCommitments.has(derived)) {
          throw new Error(
            `ECDSA public key commitment mismatch: derived commitment ${derived} is not in signerCommitments.`,
          );
        }
        signerCommitmentHex = derived;
      }
    }

    const signerCommitment = Word.fromHex(signerCommitmentHex);
    const sigBytes = signatureHexToBytes(
      cosignerSig.signature.signature,
      cosignerSig.signature.scheme,
    );
    const signature = Signature.deserialize(sigBytes);
    const txCommitment = Word.fromHex(normalizeHexWord(txCommitmentHex));
    const ecdsaPublicKey =
      cosignerSig.signature.scheme === 'ecdsa'
        ? cosignerSig.signature.publicKey
        : undefined;
    const isEcdsa = cosignerSig.signature.scheme === 'ecdsa' && Boolean(ecdsaPublicKey);

    const { key, values } = buildSignatureAdviceEntry(
      signerCommitment,
      txCommitment,
      signature,
      ecdsaPublicKey,
      isEcdsa ? cosignerSig.signature.signature : undefined,
    );
    adviceMap.insert(key, new FeltArray(values));
  }

  return adviceMap;
}

async function appendPsmAckAdvice(
  psm: PsmHttpClient,
  delta: DeltaObject,
  psmCommitmentHex: string,
  psmPublicKey: string | undefined,
  defaultAckScheme: 'falcon' | 'ecdsa',
  txCommitmentHex: string,
  adviceMap: AdviceMap,
): Promise<void> {
  const executionDelta = {
    ...delta,
    deltaPayload: delta.deltaPayload.txSummary,
  };

  const pushResult = await psm.pushDelta(executionDelta);
  const ackSigHex = pushResult.ackSig;
  if (!ackSigHex) {
    throw new Error('PSM did not return acknowledgment signature');
  }

  const psmAckScheme: 'ecdsa' | 'falcon' =
    (pushResult.ackScheme as 'ecdsa' | 'falcon') || defaultAckScheme;
  const ackPubkey = pushResult.ackPubkey || psmPublicKey;
  const normalizedPsmCommitment = normalizeHexWord(psmCommitmentHex);

  if (psmAckScheme === 'ecdsa' && ackPubkey) {
    const derived = tryComputeEcdsaCommitmentHex(ackPubkey);
    if (derived && derived !== normalizedPsmCommitment) {
      throw new Error('PSM public key commitment mismatch');
    }
  }

  const psmCommitment = Word.fromHex(normalizedPsmCommitment);
  const ackSigBytes = signatureHexToBytes(ackSigHex, psmAckScheme);
  const ackSignature = Signature.deserialize(ackSigBytes);
  const txCommitment = Word.fromHex(normalizeHexWord(txCommitmentHex));
  const isAckEcdsa = psmAckScheme === 'ecdsa' && ackPubkey;
  const { key, values } = buildSignatureAdviceEntry(
    psmCommitment,
    txCommitment,
    ackSignature,
    isAckEcdsa ? ackPubkey : undefined,
    isAckEcdsa ? ackSigHex : undefined,
  );
  adviceMap.insert(key, new FeltArray(values));
}

async function buildFinalRequest(
  params: ExecuteProposalWorkflowParams,
  saltHex: string,
  adviceMap: AdviceMap,
): Promise<TransactionRequest> {
  const metadata = params.proposal.metadata;
  const normalizedSalt = Word.fromHex(normalizeHexWord(saltHex));

  switch (metadata.proposalType) {
    case 'consume_notes': {
      if (!metadata.noteIds || metadata.noteIds.length === 0) {
        throw new Error(
          'Proposal missing noteIds. Was it created with createConsumeNotesProposal?',
        );
      }
      const { request } = await buildConsumeNotesTransactionRequest(
        params.webClient,
        metadata.noteIds,
        { salt: normalizedSalt, signatureAdviceMap: adviceMap },
      );
      return request;
    }
    case 'switch_psm': {
      if (!metadata.newPsmPubkey) {
        throw new Error(
          'Proposal missing newPsmPubkey. Was it created with createSwitchPsmProposal?',
        );
      }
      const { request } = await buildUpdatePsmTransactionRequest(
        params.webClient,
        metadata.newPsmPubkey,
        {
          salt: normalizedSalt,
          signatureAdviceMap: adviceMap,
          signatureScheme: params.signatureScheme,
        },
      );
      return request;
    }
    case 'update_procedure_threshold': {
      const { request } = await buildUpdateProcedureThresholdTransactionRequest(
        params.webClient,
        metadata.targetProcedure,
        metadata.targetThreshold,
        {
          salt: normalizedSalt,
          signatureAdviceMap: adviceMap,
          signatureScheme: params.signatureScheme,
        },
      );
      return request;
    }
    case 'p2id': {
      if (!metadata.recipientId || !metadata.faucetId || !metadata.amount) {
        throw new Error(
          'Proposal missing P2ID metadata (recipientId, faucetId, amount). Was it created with createP2idProposal?',
        );
      }
      const { request } = buildP2idTransactionRequest(
        params.accountId,
        metadata.recipientId,
        metadata.faucetId,
        BigInt(metadata.amount),
        { salt: normalizedSalt, signatureAdviceMap: adviceMap },
      );
      return request;
    }
    case 'unknown':
      throw new Error(
        'Cannot execute proposal with unknown type. The proposal must have been imported without proper metadata.',
      );
    default: {
      const { request } = await buildUpdateSignersTransactionRequest(
        params.webClient,
        metadata.targetThreshold,
        metadata.targetSignerCommitments,
        {
          salt: normalizedSalt,
          signatureAdviceMap: adviceMap,
          signatureScheme: params.signatureScheme,
        },
      );
      return request;
    }
  }
}

async function submitTransaction(
  webClient: WebClient,
  accountIdHex: string,
  request: TransactionRequest,
): Promise<void> {
  const accountId = AccountId.fromHex(accountIdHex);
  const result = await webClient.executeTransaction(accountId, request);
  const proven = await webClient.proveTransaction(result, null);
  const submissionHeight = await webClient.submitProvenTransaction(proven, result);
  await webClient.applyTransaction(result, submissionHeight);
}
