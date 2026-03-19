import type {
  DeltaObject,
  DeltaStatus,
  ProposalMetadata as GuardianProposalMetadata,
} from '@openzeppelin/guardian-client';
import type {
  ProposalMetadata,
  ProposalType,
  TransactionProposal,
  TransactionProposalSignature,
  TransactionProposalStatus,
} from '../../types.js';

export function buildGuardianMetadata(metadata: ProposalMetadata): GuardianProposalMetadata {
  const base: GuardianProposalMetadata = {
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
    case 'switch_guardian':
      return {
        ...base,
        targetThreshold: metadata.targetThreshold,
        signerCommitments: metadata.targetSignerCommitments,
        newGuardianPubkey: metadata.newGuardianPubkey,
        newGuardianEndpoint: metadata.newGuardianEndpoint,
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

export function fromGuardianMetadata(guardian: GuardianProposalMetadata): ProposalMetadata | undefined {
  if (!guardian.proposalType) return undefined;

  const base = {
    description: guardian.description ?? '',
    saltHex: guardian.salt,
  };

  switch (guardian.proposalType) {
    case 'p2id':
      return {
        ...base,
        proposalType: 'p2id',
        recipientId: guardian.recipientId ?? '',
        faucetId: guardian.faucetId ?? '',
        amount: guardian.amount ?? '0',
      };
    case 'consume_notes':
      return {
        ...base,
        proposalType: 'consume_notes',
        noteIds: guardian.noteIds ?? [],
      };
    case 'switch_guardian':
      return {
        ...base,
        proposalType: 'switch_guardian',
        newGuardianPubkey: guardian.newGuardianPubkey ?? '',
        newGuardianEndpoint: guardian.newGuardianEndpoint,
        targetThreshold: guardian.targetThreshold,
        targetSignerCommitments: guardian.signerCommitments,
      };
    case 'add_signer':
    case 'remove_signer':
    case 'change_threshold':
      return {
        ...base,
        proposalType: guardian.proposalType,
        targetThreshold: guardian.targetThreshold ?? 0,
        targetSignerCommitments: guardian.signerCommitments ?? [],
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
  return fromGuardianMetadata(delta.deltaPayload.metadata);
}

export function signatureRequirementForProposal(
  metadata: ProposalMetadata,
  defaultThreshold: number,
  getEffectiveThreshold: (proposalType: ProposalType) => number,
): number {
  if (metadata.proposalType === 'unknown') return defaultThreshold;
  return getEffectiveThreshold(metadata.proposalType);
}
