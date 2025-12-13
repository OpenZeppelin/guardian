import type {
  WebClient,
  AccountBuilder,
  AccountComponent,
  StorageSlot,
  StorageMap,
  Word,
  AccountStorageMode,
  AccountType,
} from '@demox-labs/miden-sdk';

import type { MultisigConfig, CreateAccountResult } from './types';
import { loadMultisigMasm, loadPsmMasm } from './masm-loader';

/**
 * SDK types bundle for dependency injection.
 */
export interface MidenSdkTypes {
  AccountBuilder: typeof AccountBuilder;
  AccountComponent: typeof AccountComponent;
  StorageSlot: typeof StorageSlot;
  StorageMap: typeof StorageMap;
  Word: typeof Word;
  AccountStorageMode: typeof AccountStorageMode;
  AccountType: typeof AccountType;
}

function buildMultisigStorageSlots(
  config: MultisigConfig,
  sdk: MidenSdkTypes
): StorageSlot[] {
  const { StorageSlot, StorageMap, Word } = sdk;
  const numSigners = config.signerCommitments.length;

  // Slot 0: Threshold config [threshold, num_signers, 0, 0]
  const slot0Word = new Word(
    new BigUint64Array([
      BigInt(config.threshold),
      BigInt(numSigners),
      0n,
      0n,
    ])
  );
  const slot0 = StorageSlot.fromValue(slot0Word);

  // Slot 1: Signer public keys map
  const signersMap = new StorageMap();
  config.signerCommitments.forEach((commitment, index) => {
    const key = new Word(new BigUint64Array([BigInt(index), 0n, 0n, 0n]));
    const value = Word.fromHex(commitment);
    signersMap.insert(key, value);
  });
  const slot1 = StorageSlot.map(signersMap);

  // Slot 2: Executed transactions map (empty)
  const slot2 = StorageSlot.map(new StorageMap());

  // Slot 3: Procedure threshold overrides (empty)
  const slot3 = StorageSlot.map(new StorageMap());

  return [slot0, slot1, slot2, slot3];
}

function buildPsmStorageSlots(
  config: MultisigConfig,
  sdk: MidenSdkTypes
): StorageSlot[] {
  const { StorageSlot, StorageMap, Word } = sdk;

  // Slot 0: PSM selector
  const selector = config.psmEnabled !== false ? 1n : 0n;
  const selectorWord = new Word(new BigUint64Array([selector, 0n, 0n, 0n]));
  const slot0 = StorageSlot.fromValue(selectorWord);

  // Slot 1: PSM public key map
  const psmKeyMap = new StorageMap();
  const zeroKey = new Word(new BigUint64Array([0n, 0n, 0n, 0n]));
  const psmKey = Word.fromHex(config.psmCommitment);
  psmKeyMap.insert(zeroKey, psmKey);
  const slot1 = StorageSlot.map(psmKeyMap);

  return [slot0, slot1];
}

/**
 * Creates a multisig account with PSM authentication.
 *
 * @param webClient - Initialized Miden WebClient
 * @param sdk - Miden SDK types
 * @param config - Multisig configuration
 * @returns The created account and seed
 */
export async function createMultisigAccount(
  webClient: WebClient,
  sdk: MidenSdkTypes,
  config: MultisigConfig
): Promise<CreateAccountResult> {
  // Validate configuration
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

  // Load MASM files
  const [multisigMasm, psmMasm] = await Promise.all([
    loadMultisigMasm(),
    loadPsmMasm(),
  ]);

  // Build storage slots
  const multisigSlots = buildMultisigStorageSlots(config, sdk);
  const psmSlots = buildPsmStorageSlots(config, sdk);

  // Compile PSM component with its own builder (no extra links)
  const psmBuilder = webClient.createScriptBuilder();
  const psmComponent = sdk.AccountComponent
    .compile(psmMasm, psmBuilder, psmSlots)
    .withSupportsAllTypes();

  // Compile multisig auth component; link psm as openzeppelin::psm dependency
  const multisigBuilder = webClient.createScriptBuilder();
  const psmLib = multisigBuilder.buildLibrary('openzeppelin::psm', psmMasm);
  multisigBuilder.linkStaticLibrary(psmLib);
  const multisigComponent = sdk.AccountComponent
    .compile(multisigMasm, multisigBuilder, multisigSlots)
    .withSupportsAllTypes();

  // Generate random seed
  const seed = new Uint8Array(32);
  crypto.getRandomValues(seed);

  // Build the account:
  // - Multisig as auth component
  // - PSM as regular component
  // - BasicWallet as regular component
  const accountBuilder = new sdk.AccountBuilder(seed)
    .accountType(sdk.AccountType.RegularAccountUpdatableCode)
    .storageMode(sdk.AccountStorageMode.public())
    .withAuthComponent(multisigComponent)
    .withComponent(psmComponent)
    .withBasicWalletComponent();

  const result = accountBuilder.build();

  return {
    account: result.account,
    seed,
  };
}
