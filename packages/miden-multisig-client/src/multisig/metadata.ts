import type { ProposalKind, ProposalMetadata } from '../types.js';

type RawPsmMetadata = {
  proposalType?: string;
  // Support snake_case variant just in case
  proposal_type?: string;
  targetThreshold?: number;
  targetSignerCommitments?: string[];
  saltHex?: string;
  description?: string;
  newPsmPubkey?: string;
  newPsmEndpoint?: string;
  noteIds?: string[];
  recipientId?: string;
  faucetId?: string;
  amount?: string;
} | undefined;

const VALID_KINDS: ProposalKind[] = ['add_signer', 'remove_signer', 'change_threshold', 'switch_psm', 'consume_notes', 'p2id'];

const inferKind = (raw: RawPsmMetadata): ProposalKind | undefined => {
  if (!raw) return undefined;
  // Use explicit proposalType (or snake_case proposal_type) if available and valid
  const explicitType = raw.proposalType ?? raw.proposal_type;
  if (explicitType && VALID_KINDS.includes(explicitType as ProposalKind)) {
    return explicitType as ProposalKind;
  }
  // Fall back to inference from fields
  if (raw.recipientId || raw.faucetId || raw.amount) return 'p2id';
  if (raw.noteIds && raw.noteIds.length > 0) return 'consume_notes';
  if (raw.newPsmPubkey) return 'switch_psm';
  if (raw.targetSignerCommitments) return 'change_threshold';
  return undefined;
};

export function fromPsmMetadata(raw: RawPsmMetadata): ProposalMetadata | undefined {
  if (!raw) return undefined;
  console.log('[fromPsmMetadata] raw input:', JSON.stringify(raw, null, 2));
  const kind = inferKind(raw);
  const explicitType = raw.proposalType ?? raw.proposal_type;
  console.log('[fromPsmMetadata] inferred kind:', kind, 'explicitType:', explicitType, 'raw.proposalType:', raw.proposalType, 'raw.proposal_type:', raw.proposal_type);
  if (!kind) return undefined;

  if (kind === 'p2id') {
    return {
      kind,
      description: raw.description,
      saltHex: raw.saltHex,
      recipientId: raw.recipientId ?? '',
      faucetId: raw.faucetId ?? '',
      amount: raw.amount ?? '0',
    };
  }

  if (kind === 'consume_notes') {
    return {
      kind,
      description: raw.description,
      saltHex: raw.saltHex,
      noteIds: raw.noteIds ?? [],
    };
  }

  if (kind === 'switch_psm') {
    return {
      kind,
      description: raw.description,
      saltHex: raw.saltHex,
      newPsmPubkey: raw.newPsmPubkey ?? '',
      newPsmEndpoint: raw.newPsmEndpoint,
    };
  }

  return {
    kind,
    description: raw.description,
    saltHex: raw.saltHex,
    targetThreshold: raw.targetThreshold ?? 0,
    targetSignerCommitments: raw.targetSignerCommitments ?? [],
  };
}

export function toPsmMetadata(metadata?: ProposalMetadata): Record<string, unknown> | undefined {
  if (!metadata) return undefined;
  const base = {
    proposalType: metadata.kind,
    description: metadata.description,
    saltHex: metadata.saltHex,
  };
  console.log('[toPsmMetadata] input kind:', metadata.kind, 'output proposalType:', base.proposalType);

  switch (metadata.kind) {
    case 'p2id':
      return {
        ...base,
        recipientId: metadata.recipientId,
        faucetId: metadata.faucetId,
        amount: metadata.amount,
      };
    case 'consume_notes':
      return {
        ...base,
        noteIds: metadata.noteIds,
      };
    case 'switch_psm':
      return {
        ...base,
        targetThreshold: metadata.targetThreshold,
        targetSignerCommitments: metadata.targetSignerCommitments,
        newPsmPubkey: metadata.newPsmPubkey,
        newPsmEndpoint: metadata.newPsmEndpoint,
      };
    default:
      return {
        ...base,
        targetThreshold: metadata.targetThreshold,
        targetSignerCommitments: metadata.targetSignerCommitments,
      };
  }
}

