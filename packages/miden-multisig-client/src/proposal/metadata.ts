import type { ProposalMetadata as GuardianProposalMetadata } from '@openzeppelin/guardian-client';
import type { ProposalMetadata } from '../types.js';
import { isProcedureName } from '../procedures.js';

export class ProposalMetadataCodec {
  static toGuardian(metadata: ProposalMetadata): GuardianProposalMetadata {
    const base: GuardianProposalMetadata = {
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
      case 'switch_guardian':
        return {
          ...base,
          targetThreshold: metadata.targetThreshold,
          signerCommitments: metadata.targetSignerCommitments,
          newGuardianPubkey: metadata.newGuardianPubkey,
          newGuardianEndpoint: metadata.newGuardianEndpoint,
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

  static fromGuardian(guardian?: GuardianProposalMetadata): ProposalMetadata {
    if (!guardian?.proposalType) {
      throw new Error('Missing proposal metadata.proposalType');
    }

    const base = {
      description: guardian.description ?? '',
      saltHex: guardian.salt,
      requiredSignatures: guardian.requiredSignatures,
    };

    switch (guardian.proposalType) {
      case 'p2id':
        if (!guardian.recipientId || !guardian.faucetId || !guardian.amount) {
          throw new Error('p2id proposal is missing required metadata fields');
        }
        return {
          ...base,
          proposalType: 'p2id',
          recipientId: guardian.recipientId,
          faucetId: guardian.faucetId,
          amount: guardian.amount,
        };
      case 'consume_notes':
        if (!guardian.noteIds || guardian.noteIds.length === 0) {
          throw new Error('consume_notes proposal is missing noteIds');
        }
        return {
          ...base,
          proposalType: 'consume_notes',
          noteIds: guardian.noteIds,
        };
      case 'switch_guardian':
        if (!guardian.newGuardianPubkey || !guardian.newGuardianEndpoint) {
          throw new Error('switch_guardian proposal is missing required metadata fields');
        }
        return {
          ...base,
          proposalType: 'switch_guardian',
          newGuardianPubkey: guardian.newGuardianPubkey,
          newGuardianEndpoint: guardian.newGuardianEndpoint,
          targetThreshold: guardian.targetThreshold,
          targetSignerCommitments: guardian.signerCommitments,
        };
      case 'update_procedure_threshold':
        if (guardian.targetThreshold === undefined || !guardian.targetProcedure) {
          throw new Error('update_procedure_threshold proposal is missing required metadata fields');
        }
        if (!isProcedureName(guardian.targetProcedure)) {
          throw new Error(`unknown target procedure: ${guardian.targetProcedure}`);
        }
        return {
          ...base,
          proposalType: 'update_procedure_threshold',
          targetProcedure: guardian.targetProcedure,
          targetThreshold: guardian.targetThreshold,
        };
      case 'add_signer':
      case 'remove_signer':
      case 'change_threshold':
        if (guardian.targetThreshold === undefined || !guardian.signerCommitments || guardian.signerCommitments.length === 0) {
          throw new Error(`${guardian.proposalType} proposal is missing required metadata fields`);
        }
        return {
          ...base,
          proposalType: guardian.proposalType,
          targetThreshold: guardian.targetThreshold,
          targetSignerCommitments: guardian.signerCommitments,
        };
      default:
        throw new Error(`Unsupported proposal type: ${guardian.proposalType as string}`);
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
      case 'switch_guardian':
        if (!metadata.newGuardianPubkey || !metadata.newGuardianEndpoint) {
          throw new Error('switch_guardian proposal metadata is incomplete');
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
