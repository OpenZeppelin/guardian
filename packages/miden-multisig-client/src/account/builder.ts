/**
 * Account builder for creating multisig accounts with GUARDIAN authentication.
 *
 * This module provides functionality to create multisig accounts.
 */

import {
  AccountBuilder,
  AccountComponent,
  AccountStorageMode,
  type MidenClient,
  type WasmWebClient,
} from '@miden-sdk/miden-sdk';
import type { MultisigConfig, CreateAccountResult } from '../types.js';
import { getRawMidenClient } from '../raw-client.js';
import { buildMultisigStorageSlots, buildGuardianStorageSlots } from './storage.js';
import { GUARDED_MULTISIG_ACCOUNT_COMPONENT_MASM } from './masm/account-components/auth.js';
import { normalizeSignerCommitment } from '../utils/signature.js';

/**
 * Builds the upstream guarded-multisig auth `AccountComponent` from a config, using the
 * given code builder to compile the vendored MASM. Pure with respect to network/store —
 * callers supply the raw WASM client only for its assembler.
 */
function buildGuardedMultisigComponent(
  authBuilder: Awaited<ReturnType<WasmWebClient['createCodeBuilder']>>,
  config: MultisigConfig,
): AccountComponent {
  const authSlots = [
    ...buildMultisigStorageSlots(config),
    ...buildGuardianStorageSlots(config),
  ];
  // The web SDK assembler already provides the upstream `miden::standards::auth::*` library
  // modules, so they are NOT linked here (linking would raise a duplicate-definition error).
  // Only the guarded-multisig account component itself is compiled.
  const authComponentCode = authBuilder.compileAccountComponentCode(
    GUARDED_MULTISIG_ACCOUNT_COMPONENT_MASM,
  );
  return AccountComponent.compile(authComponentCode, authSlots).withSupportsAllTypes();
}

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
  const rawClient = await getRawMidenClient(midenClient, midenRpcEndpoint);

  const authBuilder = await rawClient.createCodeBuilder();
  const authComponent = buildGuardedMultisigComponent(authBuilder, config);

  let seed = config.seed;
  // Generate random seed if not provided
  if (!seed) {
    seed = crypto.getRandomValues(new Uint8Array(32));
  }

  const storageMode = config.storageMode === 'public'
    ? AccountStorageMode.public()
    : AccountStorageMode.private();

  // Miden 0.15: the account-ID no longer encodes regular/faucet or code
  // mutability. `AccountType` collapsed to visibility (Private/Public), which is
  // set via `storageMode()`; a multisig is "regular" by virtue of not being a
  // faucet. The former `.accountType(RegularAccountUpdatableCode)` call is gone.
  const accountBuilder = new AccountBuilder(seed)
    .storageMode(storageMode)
    .withAuthComponent(authComponent)
    .withBasicWalletComponent();

  const result = accountBuilder.buildWithoutSchemaCommitment();

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
  // Upstream `AuthGuardedMultisigConfig::new` rejects a guardian equal to any approver; mirror
  // that invariant here so the TS builder cannot create an account the Rust SDK would reject.
  if (signerCommitments.has(normalizeSignerCommitment(config.guardianCommitment))) {
    throw new Error('GUARDIAN commitment must be different from all signer commitments');
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
