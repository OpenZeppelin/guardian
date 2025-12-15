/**
 * Account builder for creating multisig accounts with PSM authentication.
 *
 * This module provides functionality to create multisig accounts,
 * mirroring the Rust MultisigPsmBuilder pattern.
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
import { getMultisigMasm, getPsmMasm } from './masm.js';

// =============================================================================
// Account Creation
// =============================================================================

/**
 * Creates a multisig account with PSM authentication.
 *
 * This mirrors the Rust MultisigPsmBuilder pattern:
 * - Multisig component as auth component
 * - PSM component as regular component
 * - BasicWallet as regular component
 *
 * @param webClient - Initialized Miden WebClient
 * @param config - Multisig configuration
 * @returns The created account and seed
 */
export async function createMultisigAccount(
  webClient: WebClient,
  config: MultisigConfig
): Promise<CreateAccountResult> {
  // Validate configuration
  validateMultisigConfig(config);

  // Load MASM files
  const [multisigMasm, psmMasm] = await Promise.all([
    getMultisigMasm(),
    getPsmMasm(),
  ]);

  // Build storage slots
  const multisigSlots = buildMultisigStorageSlots(config);
  const psmSlots = buildPsmStorageSlots(config);

  // Compile PSM component with its own builder (no extra links)
  const psmBuilder = webClient.createScriptBuilder();
  const psmComponent = AccountComponent
    .compile(psmMasm, psmBuilder, psmSlots)
    .withSupportsAllTypes();

  // Compile multisig auth component; link psm as openzeppelin::psm dependency
  const multisigBuilder = webClient.createScriptBuilder();
  const psmLib = multisigBuilder.buildLibrary('openzeppelin::psm', psmMasm);
  multisigBuilder.linkStaticLibrary(psmLib);
  const multisigComponent = AccountComponent
    .compile(multisigMasm, multisigBuilder, multisigSlots)
    .withSupportsAllTypes();

  // Generate random seed
  const seed = new Uint8Array(32);
  crypto.getRandomValues(seed);

  // Build the account:
  // - Multisig as auth component
  // - PSM as regular component
  // - BasicWallet as regular component
  const accountBuilder = new AccountBuilder(seed)
    .accountType(AccountType.RegularAccountUpdatableCode)
    .storageMode(AccountStorageMode.public())
    .withAuthComponent(multisigComponent)
    .withComponent(psmComponent)
    .withBasicWalletComponent();

  const result = accountBuilder.build();

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
