import {
  AccountBuilder,
  AccountComponent,
  AccountType,
  AccountStorageMode,
  type WebClient,
} from '@demox-labs/miden-sdk';
import type { MultisigConfig, CreateAccountResult } from '../types.js';
import { buildMultisigStorageSlots, buildPsmStorageSlots } from './storage.js';
import { MULTISIG_MASM, MULTISIG_ECDSA_MASM, PSM_MASM, PSM_ECDSA_MASM } from './masm.js';

export async function createMultisigAccount(
  webClient: WebClient,
  config: MultisigConfig
): Promise<CreateAccountResult> {
  validateMultisigConfig(config);

  const signatureScheme = config.signatureScheme ?? 'falcon';
  const multisigSlots = buildMultisigStorageSlots(config);
  const psmSlots = buildPsmStorageSlots(config);

  const psmBuilder = webClient.createScriptBuilder();
  const psmMasm = signatureScheme === 'ecdsa' ? PSM_ECDSA_MASM : PSM_MASM;
  const psmComponent = AccountComponent
    .compile(psmMasm, psmBuilder, psmSlots)
    .withSupportsAllTypes();

  const multisigMasm = signatureScheme === 'ecdsa' ? MULTISIG_ECDSA_MASM : MULTISIG_MASM;
  const psmLibraryPath = signatureScheme === 'ecdsa' ? 'openzeppelin::psm_ecdsa' : 'openzeppelin::psm';
  const multisigBuilder = webClient.createScriptBuilder();
  const psmLib = multisigBuilder.buildLibrary(psmLibraryPath, psmMasm);
  multisigBuilder.linkStaticLibrary(psmLib);
  const multisigComponent = AccountComponent
    .compile(multisigMasm, multisigBuilder, multisigSlots)
    .withSupportsAllTypes();

  const seed = new Uint8Array(32);
  crypto.getRandomValues(seed);

  const storageMode = config.storageMode === 'public'
    ? AccountStorageMode.public()
    : AccountStorageMode.private();

  const accountBuilder = new AccountBuilder(seed)
    .accountType(AccountType.RegularAccountUpdatableCode)
    .storageMode(storageMode)
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

export function validateMultisigConfig(config: MultisigConfig): void {
  if (config.threshold === 0) {
    throw new Error('threshold must be greater than 0');
  }
  if (config.signerCommitments.length === 0) {
    throw new Error('at least one signer commitment is required');
  }
  for (const commitment of config.signerCommitments) {
    const stripped = commitment.startsWith('0x') || commitment.startsWith('0X')
      ? commitment.slice(2)
      : commitment;
    if (stripped.length > 64) {
      throw new Error(
        `signerCommitments must be 32-byte commitment hex (64 chars), got ${stripped.length} chars`
      );
    }
  }
  if (config.threshold > config.signerCommitments.length) {
    throw new Error(
      `threshold (${config.threshold}) cannot exceed number of signers (${config.signerCommitments.length})`
    );
  }
  if (!config.psmCommitment) {
    throw new Error('PSM commitment is required');
  }

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
