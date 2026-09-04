/**
 * Account builder for creating multisig accounts with GUARDIAN authentication.
 *
 * This module provides functionality to create multisig accounts.
 */

import {
  AccountBuilder,
  AccountComponent,
  AccountStorageMode,
  AuthGuardedMultisigConfig,
  ProcedureThreshold,
  Word,
  createAuthGuardedMultisig,
  type MidenClient,
} from "@miden-sdk/miden-sdk";
import { getProcedureRoot } from "../procedures.js";
import type { MultisigConfig, CreateAccountResult } from "../types.js";
import { normalizeSignerCommitment } from "../utils/signature.js";

/**
 * Discriminants of the SDK's wasm `AuthScheme` enum, which `AuthGuardedMultisigConfig` takes.
 *
 * They are inlined rather than imported because the package root exports two different things
 * named `AuthScheme`: the wasm enum, and a string-valued convenience const from `api-types`
 * that is re-exported second and therefore wins. Importing the name yields the string const,
 * whose members are `undefined` here, and the constructor rejects that as an invalid enum value.
 * Source: `crates/web-client/src/models/auth_scheme.rs`.
 */
const AUTH_SCHEME = { ecdsa: 1, falcon: 2 } as const;

/**
 * Builds the guarded-multisig component from the upstream standard component.
 *
 * Compiling an equivalent MASM source ourselves would link the standards package dynamically and
 * yield a different `auth_tx` root, which `AccountComponentInterface::from_procedures` cannot
 * classify — the client then declines to attach fee conversion info and every transaction fails
 * on a fee-charging chain.
 *
 * Per-procedure thresholds are keyed by procedure root. The roots come from the pinned table,
 * not from the component: a `ProcedureName` is this package's own vocabulary and does not match
 * the component's MASM export names (`update_signers` is exported as `update_signers_and_threshold`),
 * and `send_asset`/`receive_asset` are `BasicWallet` procedures that the auth component never
 * exports at all. `procedure_roots_match_upstream_component` pins the table to the component.
 */
function buildGuardedMultisigComponent(
  config: MultisigConfig,
): AccountComponent {
  const approvers = config.signerCommitments.map((commitment) =>
    Word.fromHex(normalizeSignerCommitment(commitment)),
  );
  const guardian = Word.fromHex(
    normalizeSignerCommitment(config.guardianCommitment),
  );
  const scheme =
    AUTH_SCHEME[config.signatureScheme === "ecdsa" ? "ecdsa" : "falcon"];

  const baseConfig = new AuthGuardedMultisigConfig(
    approvers,
    config.threshold,
    guardian,
    scheme,
  );

  if (!config.procedureThresholds?.length) {
    return createAuthGuardedMultisig(baseConfig).withSupportsAllTypes();
  }

  const thresholds = config.procedureThresholds.map(
    (entry) =>
      new ProcedureThreshold(
        Word.fromHex(getProcedureRoot(entry.procedure)),
        entry.threshold,
      ),
  );

  return createAuthGuardedMultisig(
    baseConfig.withProcThresholds(thresholds),
  ).withSupportsAllTypes();
}

/**
 * Creates a multisig account with GUARDIAN authentication.
 *
 * @param midenClient - Initialized MidenClient
 * @param config - Multisig configuration
 * @param midenRpcEndpoint - RPC endpoint for the MidenClient's network
 * @returns The created account and seed
 */
export async function createMultisigAccount(
  midenClient: MidenClient,
  config: MultisigConfig,
  midenRpcEndpoint: string,
): Promise<CreateAccountResult> {
  validateMultisigConfig(config);
  const authComponent = buildGuardedMultisigComponent(config);

  let seed = config.seed;
  // Generate random seed if not provided
  if (!seed) {
    seed = crypto.getRandomValues(new Uint8Array(32));
  }

  const storageMode =
    config.storageMode === "public"
      ? AccountStorageMode.public()
      : AccountStorageMode.private();

  // Miden 0.15: the account-ID no longer encodes account type; visibility is set
  // via `storageMode()`, so the former `.accountType(...)` call is gone.
  const accountBuilder = new AccountBuilder(seed)
    .storageMode(storageMode)
    .withAuthComponent(authComponent)
    .withBasicWalletComponent();

  const result = accountBuilder.buildWithoutSchemaCommitment();

  await midenClient.accounts.insert({
    account: result.account,
    overwrite: false,
  });

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
    throw new Error("threshold must be greater than 0");
  }
  if (config.signerCommitments.length === 0) {
    throw new Error("at least one signer commitment is required");
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
      `threshold (${config.threshold}) cannot exceed number of signers (${config.signerCommitments.length})`,
    );
  }
  if (!config.guardianCommitment) {
    throw new Error("GUARDIAN commitment is required");
  }
  if (
    signerCommitments.has(normalizeSignerCommitment(config.guardianCommitment))
  ) {
    throw new Error(
      "GUARDIAN commitment must be different from all signer commitments",
    );
  }

  // Validate procedure thresholds if provided
  if (config.procedureThresholds) {
    const seen = new Set<string>();
    for (const pt of config.procedureThresholds) {
      if (pt.threshold < 1) {
        throw new Error("procedure threshold must be at least 1");
      }
      if (pt.threshold > config.signerCommitments.length) {
        throw new Error(
          `procedure threshold (${pt.threshold}) cannot exceed number of signers (${config.signerCommitments.length})`,
        );
      }

      if (seen.has(pt.procedure)) {
        throw new Error(`duplicate procedure threshold for: ${pt.procedure}`);
      }
      seen.add(pt.procedure);
    }

    // An override is only enforceable if lowering it costs at least as many
    // signatures as the override itself demands. `update_procedure_threshold`
    // is the procedure that edits overrides, so anything above its own
    // effective threshold can be lowered by a smaller quorum and then used:
    // a 2-of-5 with `send_asset: 4` is a 2-of-5 spend lock, not a 4-of-5 one.
    // Mirrors `AuthMultisig::new` in miden-standards, which rejects the same
    // shape, so Rust cannot build an account TypeScript would otherwise allow.
    const setterOverride = config.procedureThresholds.find(
      (pt) => pt.procedure === "update_procedure_threshold",
    )?.threshold;
    const setterThreshold = setterOverride ?? config.threshold;

    for (const pt of config.procedureThresholds) {
      if (pt.threshold > setterThreshold) {
        throw new Error(
          `procedure threshold override for ${pt.procedure} (${pt.threshold}) exceeds the ` +
            `threshold of ${setterThreshold} that guards update_procedure_threshold; such an ` +
            `override can be removed by a smaller quorum. Raise the update_procedure_threshold ` +
            `override to at least ${pt.threshold} to make it enforceable`,
        );
      }
    }
  }
}
