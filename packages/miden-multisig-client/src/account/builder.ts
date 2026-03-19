/**
 * Account builder for creating multisig accounts with PSM authentication.
 *
 * This module provides functionality to create multisig accounts.
 */

import {
  AccountBuilder,
  AccountComponent,
  AccountType,
  AccountStorageMode,
  type WebClient,
} from '@miden-sdk/miden-sdk';
import type { MultisigConfig, CreateAccountResult } from '../types.js';
import { buildMultisigStorageSlots, buildPsmStorageSlots } from './storage.js';
import {
  MULTISIG_ECDSA_MASM,
  MULTISIG_MASM,
  PSM_ECDSA_MASM,
  PSM_MASM,
} from './masm/auth.js';
import {
  MULTISIG_PSM_ACCOUNT_COMPONENT_MASM,
  MULTISIG_PSM_ECDSA_ACCOUNT_COMPONENT_MASM,
} from './masm/account-components/auth.js';
import { normalizeSignerCommitment } from '../utils/signature.js';

/**
 * Creates a multisig account with PSM authentication.
 *
 * @param webClient - Initialized Miden WebClient
 * @param config - Multisig configuration
 * @returns The created account and seed
 */
export async function createMultisigAccount(
  webClient: WebClient,
  config: MultisigConfig
): Promise<CreateAccountResult> {
  validateMultisigConfig(config);
  const signatureScheme = config.signatureScheme ?? 'falcon';
  const authSlots = [
    ...buildMultisigStorageSlots(config),
    ...buildPsmStorageSlots(config),
  ];
  const psmMasm = signatureScheme === 'ecdsa' ? PSM_ECDSA_MASM : PSM_MASM;
  const multisigMasm = signatureScheme === 'ecdsa' ? MULTISIG_ECDSA_MASM : MULTISIG_MASM;
  const authComponentMasm = signatureScheme === 'ecdsa'
    ? MULTISIG_PSM_ECDSA_ACCOUNT_COMPONENT_MASM
    : MULTISIG_PSM_ACCOUNT_COMPONENT_MASM;
  const psmLibraryPath = signatureScheme === 'ecdsa'
    ? 'openzeppelin::auth::psm_ecdsa'
    : 'openzeppelin::auth::psm';
  const multisigLibraryPath = signatureScheme === 'ecdsa'
    ? 'openzeppelin::auth::multisig_ecdsa'
    : 'openzeppelin::auth::multisig';

  const authBuilder = webClient.createCodeBuilder();
  const psmLib = authBuilder.buildLibrary(psmLibraryPath, psmMasm);
  authBuilder.linkStaticLibrary(psmLib);
  const multisigLib = authBuilder.buildLibrary(multisigLibraryPath, multisigMasm);
  authBuilder.linkStaticLibrary(multisigLib);
  const authCode = authBuilder.compileAccountComponentCode(authComponentMasm);
  const authComponent = AccountComponent
    .compile(authCode, authSlots)
    .withSupportsAllTypes();

  // Generate random seed
  const seed = new Uint8Array(32);
  crypto.getRandomValues(seed);

  const storageMode = config.storageMode === 'public'
    ? AccountStorageMode.public()
    : AccountStorageMode.private();

  const accountBuilder = new AccountBuilder(seed)
    .accountType(AccountType.RegularAccountUpdatableCode)
    .storageMode(storageMode)
    .withAuthComponent(authComponent)
    .withBasicWalletComponent();

  const result = accountBuilder.build();

  await webClient.newAccount(result.account, false);

  return {
    account: result.account,
    seed,
  };
}

/**
 * Validates a multisig configuration.
 *
 * @param config - The configuration to validate
 * @throws Error if configuration is invalid
 */
export function validateMultisigConfig(config: MultisigConfig): void {
  if (config.threshold === 0) {
    throw new Error('threshold must be greater than 0');
  }
  if (config.signerCommitments.length === 0) {
    throw new Error('at least one signer commitment is required');
  }

  const signerCommitments = new Set<string>();
  for (const signerCommitment of config.signerCommitments) {
    const normalizedCommitment = normalizeSignerCommitment(signerCommitment);
    if (signerCommitments.has(normalizedCommitment)) {
      throw new Error(`duplicate signer commitment: ${normalizedCommitment}`);
    }
    signerCommitments.add(normalizedCommitment);
  }

  if (config.threshold > config.signerCommitments.length) {
    throw new Error(
      `threshold (${config.threshold}) cannot exceed number of signers (${config.signerCommitments.length})`
    );
  }
  if (!config.psmCommitment) {
    throw new Error('PSM commitment is required');
  }

  // Validate procedure thresholds if provided
  if (config.procedureThresholds) {
    const seen = new Set<string>();
    for (const pt of config.procedureThresholds) {
      if (pt.threshold < 1) {
        throw new Error('procedure threshold must be at least 1');
      }
      if (pt.threshold > config.signerCommitments.length) {
        throw new Error(
          `procedure threshold (${pt.threshold}) cannot exceed number of signers (${config.signerCommitments.length})`
        );
      }

      if (seen.has(pt.procedure)) {
        throw new Error(`duplicate procedure threshold for: ${pt.procedure}`);
      }
      seen.add(pt.procedure);
    }
  }
}
