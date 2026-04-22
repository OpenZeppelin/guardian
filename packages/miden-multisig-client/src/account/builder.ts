/**
 * Account builder for creating multisig accounts with GUARDIAN authentication.
 *
 * This module provides functionality to create multisig accounts.
 */

import {
  AccountBuilder,
  AccountComponent,
  AccountType,
  AccountStorageMode,
  type MidenClient,
} from '@miden-sdk/miden-sdk';
import type { MultisigConfig, CreateAccountResult } from '../types.js';
import { getRawMidenClient } from '../raw-client.js';
import { buildMultisigStorageSlots, buildGuardianStorageSlots } from './storage.js';
import {
  MULTISIG_ECDSA_MASM,
  MULTISIG_MASM,
  GUARDIAN_ECDSA_MASM,
  GUARDIAN_MASM,
} from './masm/auth.js';
import {
  MULTISIG_GUARDIAN_ACCOUNT_COMPONENT_MASM,
  MULTISIG_GUARDIAN_ECDSA_ACCOUNT_COMPONENT_MASM,
} from './masm/account-components/auth.js';
import { normalizeSignerCommitment } from '../utils/signature.js';

/**
 * Creates a multisig account with GUARDIAN authentication.
 *
 * @param midenClient - Initialized MidenClient
 * @param config - Multisig configuration
 * @returns The created account and seed
 */
export async function createMultisigAccount(
  midenClient: MidenClient,
  config: MultisigConfig,
  midenRpcEndpoint?: string,
): Promise<CreateAccountResult> {
  validateMultisigConfig(config);
  const signatureScheme = config.signatureScheme ?? 'falcon';
  const rawClient = await getRawMidenClient(midenClient, midenRpcEndpoint);
  const authSlots = [
    ...buildMultisigStorageSlots(config),
    ...buildGuardianStorageSlots(config),
  ];
  const guardianMasm = signatureScheme === 'ecdsa' ? GUARDIAN_ECDSA_MASM : GUARDIAN_MASM;
  const multisigMasm = signatureScheme === 'ecdsa' ? MULTISIG_ECDSA_MASM : MULTISIG_MASM;
  const authComponentMasm = signatureScheme === 'ecdsa'
    ? MULTISIG_GUARDIAN_ECDSA_ACCOUNT_COMPONENT_MASM
    : MULTISIG_GUARDIAN_ACCOUNT_COMPONENT_MASM;
  const guardianLibraryPath = signatureScheme === 'ecdsa'
    ? 'openzeppelin::auth::guardian_ecdsa'
    : 'openzeppelin::auth::guardian';
  const multisigLibraryPath = signatureScheme === 'ecdsa'
    ? 'openzeppelin::auth::multisig_ecdsa'
    : 'openzeppelin::auth::multisig';

  const authBuilder = rawClient.createCodeBuilder();
  authBuilder.linkModule(guardianLibraryPath, guardianMasm);
  authBuilder.linkModule(multisigLibraryPath, multisigMasm);
  const authComponentCode = authBuilder.compileAccountComponentCode(authComponentMasm);
  const authComponent = AccountComponent
    .compile(authComponentCode, authSlots)
    .withSupportsAllTypes();

  let seed = config.seed;
  // Generate random seed if not provided
  if (!seed) {
    seed = crypto.getRandomValues(new Uint8Array(32));
  }

  const storageMode = config.storageMode === 'public'
    ? AccountStorageMode.public()
    : AccountStorageMode.private();

  const accountBuilder = new AccountBuilder(seed)
    .accountType(AccountType.RegularAccountUpdatableCode)
    .storageMode(storageMode)
    .withAuthComponent(authComponent)
    .withBasicWalletComponent();

  const result = accountBuilder.build();

  await midenClient.accounts.insert({ account: result.account, overwrite: false });

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
  if (!config.guardianCommitment) {
    throw new Error('GUARDIAN commitment is required');
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
