import type {
  DeltaObject,
  DeltaStatus,
  ProposalMetadata as PsmProposalMetadata,
} from '@openzeppelin/psm-client';
import type {
  ProposalMetadata,
  ProposalType,
  TransactionProposal,
  TransactionProposalSignature,
  TransactionProposalStatus,
} from '../../types.js';

export function buildPsmMetadata(metadata: ProposalMetadata): PsmProposalMetadata {
  const base: PsmProposalMetadata = {
    proposalType: metadata.proposalType,
    description: metadata.description,
    salt: metadata.saltHex,
  };

  switch (metadata.proposalType) {
    case 'consume_notes':
      return {
        ...base,
        noteIds: metadata.noteIds,
      };
    case 'p2id':
      return {
        ...base,
        recipientId: metadata.recipientId,
        faucetId: metadata.faucetId,
        amount: metadata.amount,
      };
    case 'switch_psm':
      return {
        ...base,
        targetThreshold: metadata.targetThreshold,
        signerCommitments: metadata.targetSignerCommitments,
        newPsmPubkey: metadata.newPsmPubkey,
        newPsmEndpoint: metadata.newPsmEndpoint,
      };
    case 'add_signer':
    case 'remove_signer':
    case 'change_threshold':
      return {
        ...base,
        targetThreshold: metadata.targetThreshold,
        signerCommitments: metadata.targetSignerCommitments,
      };
    case 'unknown':
      return base;
    default:
      return base;
  }
}

export function fromPsmMetadata(psm: PsmProposalMetadata): ProposalMetadata | undefined {
  if (!psm.proposalType) return undefined;

  const base = {
    description: psm.description ?? '',
    saltHex: psm.salt,
  };

  switch (psm.proposalType) {
    case 'p2id':
      return {
        ...base,
        proposalType: 'p2id',
        recipientId: psm.recipientId ?? '',
        faucetId: psm.faucetId ?? '',
        amount: psm.amount ?? '0',
      };
    case 'consume_notes':
      return {
        ...base,
        proposalType: 'consume_notes',
        noteIds: psm.noteIds ?? [],
      };
    case 'switch_psm':
      return {
        ...base,
        proposalType: 'switch_psm',
        newPsmPubkey: psm.newPsmPubkey ?? '',
        newPsmEndpoint: psm.newPsmEndpoint,
        targetThreshold: psm.targetThreshold,
        targetSignerCommitments: psm.signerCommitments,
      };
    case 'add_signer':
    case 'remove_signer':
    case 'change_threshold':
      return {
        ...base,
        proposalType: psm.proposalType,
        targetThreshold: psm.targetThreshold ?? 0,
        targetSignerCommitments: psm.signerCommitments ?? [],
      };
    default:
      return undefined;
  }
}

export function deltaStatusToProposalStatus(
  status: DeltaStatus,
  signaturesRequired: number,
): TransactionProposalStatus {
  switch (status.status) {
    case 'pending': {
      const signaturesCollected = status.cosignerSigs.length;
      if (signaturesCollected >= signaturesRequired) {
        return { type: 'ready' };
      }
      return {
        type: 'pending',
        signaturesCollected,
        signaturesRequired,
        signers: status.cosignerSigs.map((s) => s.signerId),
      };
    }
    case 'candidate':
      return { type: 'ready' };
    case 'canonical':
    case 'discarded':
      return { type: 'finalized' };
  }
}

interface DeltaToProposalParams {
  delta: DeltaObject;
  proposalId: string;
  metadata: ProposalMetadata;
  signaturesRequired: number;
  existingSignatures?: TransactionProposalSignature[];
}

export function deltaToProposal({
  delta,
  proposalId,
  metadata,
  signaturesRequired,
  existingSignatures,
}: DeltaToProposalParams): TransactionProposal {
  const status = deltaStatusToProposalStatus(delta.status, signaturesRequired);

  const signaturesFromStatus =
    delta.status.status === 'pending'
      ? delta.status.cosignerSigs.map((s) => ({
          signerId: s.signerId,
          signature: s.signature,
          timestamp: s.timestamp,
        }))
      : [];

  const signaturesMap = new Map<string, TransactionProposalSignature>();
  for (const sig of existingSignatures ?? []) {
    signaturesMap.set(sig.signerId, sig);
  }
  for (const sig of signaturesFromStatus) {
    signaturesMap.set(sig.signerId, sig);
  }
  const signatures = Array.from(signaturesMap.values());

  return {
    id: proposalId,
    commitment: proposalId,
    accountId: delta.accountId,
    nonce: delta.nonce,
    status,
    txSummary: delta.deltaPayload.txSummary.data,
    signatures,
    metadata,
  };
}

export function resolveMetadata(
  delta: DeltaObject,
  existingMetadata?: ProposalMetadata,
): ProposalMetadata | undefined {
  if (existingMetadata) return existingMetadata;
  if (!delta.deltaPayload.metadata) return undefined;
  return fromPsmMetadata(delta.deltaPayload.metadata);
}

export function signatureRequirementForProposal(
  metadata: ProposalMetadata,
  defaultThreshold: number,
  getEffectiveThreshold: (proposalType: ProposalType) => number,
): number {
  if (metadata.proposalType === 'unknown') return defaultThreshold;
  return getEffectiveThreshold(metadata.proposalType);
}
