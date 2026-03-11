import type { ProposalMetadata as PsmProposalMetadata } from '@openzeppelin/psm-client';
import type { ProposalMetadata } from '../types.js';
import { isProcedureName } from '../procedures.js';

export class ProposalMetadataCodec {
  static toPsm(metadata: ProposalMetadata): PsmProposalMetadata {
    const base: PsmProposalMetadata = {
      proposalType: metadata.proposalType,
      description: metadata.description,
      salt: metadata.saltHex,
      requiredSignatures: metadata.requiredSignatures,
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
      case 'update_procedure_threshold':
        return {
          ...base,
          targetThreshold: metadata.targetThreshold,
          targetProcedure: metadata.targetProcedure,
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
    }
  }

  static fromPsm(psm?: PsmProposalMetadata): ProposalMetadata {
    if (!psm?.proposalType) {
      throw new Error('Missing proposal metadata.proposalType');
    }

    const base = {
      description: psm.description ?? '',
      saltHex: psm.salt,
      requiredSignatures: psm.requiredSignatures,
    };

    switch (psm.proposalType) {
      case 'p2id':
        if (!psm.recipientId || !psm.faucetId || !psm.amount) {
          throw new Error('p2id proposal is missing required metadata fields');
        }
        return {
          ...base,
          proposalType: 'p2id',
          recipientId: psm.recipientId,
          faucetId: psm.faucetId,
          amount: psm.amount,
        };
      case 'consume_notes':
        if (!psm.noteIds || psm.noteIds.length === 0) {
          throw new Error('consume_notes proposal is missing noteIds');
        }
        return {
          ...base,
          proposalType: 'consume_notes',
          noteIds: psm.noteIds,
        };
      case 'switch_psm':
        if (!psm.newPsmPubkey || !psm.newPsmEndpoint) {
          throw new Error('switch_psm proposal is missing required metadata fields');
        }
        return {
          ...base,
          proposalType: 'switch_psm',
          newPsmPubkey: psm.newPsmPubkey,
          newPsmEndpoint: psm.newPsmEndpoint,
          targetThreshold: psm.targetThreshold,
          targetSignerCommitments: psm.signerCommitments,
        };
      case 'update_procedure_threshold':
        if (psm.targetThreshold === undefined || !psm.targetProcedure) {
          throw new Error('update_procedure_threshold proposal is missing required metadata fields');
        }
        if (!isProcedureName(psm.targetProcedure)) {
          throw new Error(`unknown target procedure: ${psm.targetProcedure}`);
        }
        return {
          ...base,
          proposalType: 'update_procedure_threshold',
          targetProcedure: psm.targetProcedure,
          targetThreshold: psm.targetThreshold,
        };
      case 'add_signer':
      case 'remove_signer':
      case 'change_threshold':
        if (psm.targetThreshold === undefined || !psm.signerCommitments || psm.signerCommitments.length === 0) {
          throw new Error(`${psm.proposalType} proposal is missing required metadata fields`);
        }
        return {
          ...base,
          proposalType: psm.proposalType,
          targetThreshold: psm.targetThreshold,
          targetSignerCommitments: psm.signerCommitments,
        };
      default:
        throw new Error(`Unsupported proposal type: ${psm.proposalType as string}`);
    }
  }

  static validate(metadata: ProposalMetadata): ProposalMetadata {
    switch (metadata.proposalType) {
      case 'add_signer':
      case 'remove_signer':
      case 'change_threshold':
        if (
          metadata.targetThreshold === undefined ||
          !metadata.targetSignerCommitments ||
          metadata.targetSignerCommitments.length === 0
        ) {
          throw new Error(`${metadata.proposalType} proposal metadata is incomplete`);
        }
        return metadata;
      case 'switch_psm':
        if (!metadata.newPsmPubkey || !metadata.newPsmEndpoint) {
          throw new Error('switch_psm proposal metadata is incomplete');
        }
        return metadata;
      case 'update_procedure_threshold':
        if (!metadata.targetProcedure || metadata.targetThreshold === undefined) {
          throw new Error('update_procedure_threshold proposal metadata is incomplete');
        }
        return metadata;
      case 'consume_notes':
        if (!metadata.noteIds || metadata.noteIds.length === 0) {
          throw new Error('consume_notes proposal metadata is incomplete');
        }
        return metadata;
      case 'p2id':
        if (!metadata.recipientId || !metadata.faucetId || !metadata.amount) {
          throw new Error('p2id proposal metadata is incomplete');
        }
        return metadata;
      case 'unknown':
        throw new Error('unknown proposal type is not supported');
    }
  }
}
