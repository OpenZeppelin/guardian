import type { DeltaObject, DeltaStatus } from '@openzeppelin/psm-client';
import type {
  ExportedProposal,
  Proposal,
  ProposalMetadata,
  ProposalSignatureEntry,
  ProposalStatus,
  ProposalType,
} from '../types.js';
import { computeCommitmentFromTxSummary } from '../multisig/helpers.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { ProposalMetadataCodec } from './metadata.js';
import { ProposalSignatures } from './signatures.js';

interface ProposalFactoryOptions {
  accountId: string;
  signerCommitments: string[];
  resolveRequiredSignatures: (proposalType: ProposalType) => number;
}

export class ProposalFactory {
  constructor(private readonly options: ProposalFactoryOptions) {}

  assertAccountId(accountId: string): void {
    if (accountId.toLowerCase() === this.options.accountId.toLowerCase()) {
      return;
    }

    throw new Error(`Proposal is for a different account: ${accountId}`);
  }

  assertCommitmentMatchesTxSummary(
    commitment: string,
    txSummaryBase64: string,
    context: string,
  ): string {
    const expectedCommitment = normalizeHexWord(computeCommitmentFromTxSummary(txSummaryBase64));
    const actualCommitment = normalizeHexWord(commitment);

    if (actualCommitment !== expectedCommitment) {
      throw new Error(
        `${context}: commitment ${actualCommitment} does not match tx_summary ${expectedCommitment}`,
      );
    }

    return expectedCommitment;
  }

  fromDelta(
    delta: DeltaObject,
    proposalId: string,
    metadata?: ProposalMetadata,
    existingSignatures: ProposalSignatureEntry[] = [],
  ): Proposal {
    this.assertAccountId(delta.accountId);
    const normalizedProposalId = this.assertCommitmentMatchesTxSummary(
      proposalId,
      delta.deltaPayload.txSummary.data,
      'Invalid proposal',
    );

    const resolvedMetadata = ProposalMetadataCodec.validate(
      metadata ?? ProposalMetadataCodec.fromPsm(delta.deltaPayload.metadata),
    );

    const localSignatures = new ProposalSignatures(
      existingSignatures,
      this.options.signerCommitments,
      `Invalid local proposal signatures for ${normalizedProposalId}`,
    ).entries();

    const serverSignatures =
      delta.status.status === 'pending'
        ? new ProposalSignatures(
            delta.status.cosignerSigs.map((signature) => ({
              signerId: signature.signerId,
              signature: signature.signature,
              timestamp: signature.timestamp,
            })),
            this.options.signerCommitments,
            `Invalid server proposal signatures for ${normalizedProposalId}`,
          ).entries()
        : [];

    const signatures = ProposalSignatures.mergeEntries([localSignatures, serverSignatures]);

    return {
      id: normalizedProposalId,
      accountId: delta.accountId,
      nonce: delta.nonce,
      status: this.toStatus(delta.status, resolvedMetadata.proposalType, signatures),
      txSummary: delta.deltaPayload.txSummary.data,
      signatures,
      metadata: resolvedMetadata,
    };
  }

  fromExported(exported: ExportedProposal): Proposal {
    if (typeof exported.nonce !== 'number' || !Number.isInteger(exported.nonce) || exported.nonce < 0) {
      throw new Error('Invalid proposal JSON: nonce is required');
    }

    this.assertAccountId(exported.accountId);

    const normalizedCommitment = this.assertCommitmentMatchesTxSummary(
      exported.commitment,
      exported.txSummaryBase64,
      'Invalid proposal',
    );

    const metadata = ProposalMetadataCodec.validate(exported.metadata);
    const signatures = new ProposalSignatures(
      exported.signatures.map((signature) => {
        const scheme = signature.scheme ?? 'falcon';
        if (scheme === 'ecdsa' && !signature.publicKey) {
          throw new Error(
            `Invalid imported proposal signatures: ECDSA signature for ${signature.commitment} is missing publicKey`,
          );
        }

        return {
          signerId: signature.commitment,
          signature:
            scheme === 'ecdsa'
              ? {
                  scheme,
                  signature: signature.signatureHex,
                  publicKey: signature.publicKey,
                }
              : {
                  scheme,
                  signature: signature.signatureHex,
                },
          timestamp: signature.timestamp ?? new Date().toISOString(),
        };
      }),
      this.options.signerCommitments,
      'Invalid imported proposal signatures',
    ).entries();

    const signaturesRequired = this.options.resolveRequiredSignatures(metadata.proposalType);

    return {
      id: normalizedCommitment,
      accountId: exported.accountId,
      nonce: exported.nonce,
      status: signatures.length >= signaturesRequired ? 'ready' : 'pending',
      txSummary: exported.txSummaryBase64,
      signatures,
      metadata,
    };
  }

  private toStatus(
    status: DeltaStatus,
    proposalType: ProposalType,
    signatures: ProposalSignatureEntry[],
  ): ProposalStatus {
    switch (status.status) {
      case 'pending': {
        const signaturesRequired = this.options.resolveRequiredSignatures(proposalType);
        return signatures.length >= signaturesRequired ? 'ready' : 'pending';
      }
      case 'candidate':
        return 'ready';
      case 'canonical':
      case 'discarded':
        return 'finalized';
    }
  }
}
