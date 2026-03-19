/**
 * Procedure-related helpers for the web UI.
 */

import type { ProcedureName, ProposalType } from '@openzeppelin/miden-multisig-client';

/**
 * Information about a user-facing procedure.
 */
export interface ProcedureInfo {
  name: ProcedureName;
  label: string;
  description: string;
}

/**
 * User-facing procedures that can have threshold overrides.
 */
export const USER_PROCEDURES: ProcedureInfo[] = [
  { name: 'receive_asset', label: 'Receive Assets', description: 'Accept incoming assets' },
  { name: 'send_asset', label: 'Send Assets', description: 'Send assets to other accounts' },
  { name: 'update_signers', label: 'Update Signers', description: 'Add/remove signers or change threshold' },
  { name: 'update_procedure_threshold', label: 'Update Procedure Threshold', description: 'Set or clear threshold overrides' },
  { name: 'update_guardian', label: 'Update GUARDIAN', description: 'Change GUARDIAN server configuration' },
];

/**
 * Maps a proposal type to the procedure that determines its threshold.
 *
 * @param proposalType - The type of proposal
 * @returns The procedure name that controls this proposal's threshold, or null if not mapped
 */
export function getProposalProcedure(proposalType: ProposalType): ProcedureName | null {
  switch (proposalType) {
    case 'p2id':
      return 'send_asset';
    case 'consume_notes':
      return 'receive_asset';
    case 'add_signer':
    case 'remove_signer':
    case 'change_threshold':
      return 'update_signers';
    case 'update_procedure_threshold':
      return 'update_procedure_threshold';
    case 'switch_guardian':
      return 'update_guardian';
    default:
        return null;
  }
}

/**
 * Get the effective threshold for a given proposal type.
 *
 * @param proposalType - The type of proposal
 * @param defaultThreshold - The default multisig threshold
 * @param procedureThresholds - Optional map of procedure-specific thresholds
 * @returns The threshold that applies to this proposal type
 */
export function getEffectiveThreshold(
  proposalType: ProposalType,
  defaultThreshold: number,
  procedureThresholds?: Map<ProcedureName, number>,
): number {
  if (!procedureThresholds || procedureThresholds.size === 0) {
    return defaultThreshold;
  }

  const procedure = getProposalProcedure(proposalType);
  if (!procedure) {
    return defaultThreshold;
  }

  return procedureThresholds.get(procedure) ?? defaultThreshold;
}
