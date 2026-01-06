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
} from '@demox-labs/miden-sdk';
import type { MultisigConfig, CreateAccountResult } from '../types.js';
import { buildMultisigStorageSlots, buildPsmStorageSlots } from './storage.js';
import { MULTISIG_MASM, PSM_MASM } from './masm.js';

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

  const multisigSlots = buildMultisigStorageSlots(config);
  const psmSlots = buildPsmStorageSlots(config);

  const psmBuilder = webClient.createScriptBuilder();
  const psmComponent = AccountComponent
    .compile(PSM_MASM, psmBuilder, psmSlots)
    .withSupportsAllTypes();

  const multisigBuilder = webClient.createScriptBuilder();
  const psmLib = multisigBuilder.buildLibrary('openzeppelin::psm', PSM_MASM);
  multisigBuilder.linkStaticLibrary(psmLib);
  const multisigComponent = AccountComponent
    .compile(MULTISIG_MASM, multisigBuilder, multisigSlots)
    .withSupportsAllTypes();

  // Generate random seed
  const seed = new Uint8Array(32);
  crypto.getRandomValues(seed);

  const accountBuilder = new AccountBuilder(seed)
    .accountType(AccountType.RegularAccountUpdatableCode)
    .storageMode(AccountStorageMode.public())
    .withAuthComponent(multisigComponent)
    .withComponent(psmComponent)
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
  if (config.threshold > config.signerCommitments.length) {
    throw new Error(
      `threshold (${config.threshold}) cannot exceed number of signers (${config.signerCommitments.length})`
    );
  }
  if (!config.psmCommitment) {
    throw new Error('PSM commitment is required');
  }
}
