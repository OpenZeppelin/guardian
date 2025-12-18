import type {
  WebClient,
  TransactionRequest,
  TransactionSummary,
  TransactionScript,
} from '@demox-labs/miden-sdk';
import {
  AccountId,
  AdviceMap,
  Felt,
  FeltArray,
  Rpo256,
  Signature,
  TransactionRequestBuilder,
  Word,
} from '@demox-labs/miden-sdk';

import { getMultisigMasm, getPsmMasm } from './account/masm.js';

/**
 * Execute a transaction up to the Unauthorized halt and return the summary.
 *
 * Uses the Rust `executeForSummary` WASM binding which returns the
 * TransactionSummary directly when the transaction is unauthorized.
 */
export async function executeForSummary(
  client: WebClient,
  accountId: string,
  txRequest: TransactionRequest,
): Promise<TransactionSummary> {
  const acc = AccountId.fromHex(accountId);
  // The Rust binding returns TransactionSummary directly on Unauthorized
  return (client as any).executeForSummary(acc, txRequest);
}

// =============================================================================
// Transaction builders (update_signers)
// =============================================================================

/**
 * Normalize a hex string for Word.fromHex.
 * Ensures 0x prefix and lowercase, pads to 64 characters (32 bytes).
 */
export function normalizeHexWord(hex: string): string {
  let clean = hex.startsWith('0x') || hex.startsWith('0X') ? hex.slice(2) : hex;
  clean = clean.toLowerCase();
  // Pad to 64 hex chars (32 bytes = 256 bits = 4 field elements)
  clean = clean.padStart(64, '0');
  return `0x${clean}`;
}

/**
 * Build the multisig config advice payload and hash.
 */
function buildMultisigConfigAdvice(
  threshold: number,
  signerCommitments: string[],
): { configHash: Word; payload: FeltArray } {
  const numApprovers = signerCommitments.length;
  const felts: Felt[] = [
    new Felt(BigInt(threshold)),
    new Felt(BigInt(numApprovers)),
    new Felt(0n),
    new Felt(0n),
  ];
  // Commitments are appended in reverse order, matching Rust builder
  for (const commitment of [...signerCommitments].reverse()) {
    const word = Word.fromHex(normalizeHexWord(commitment));
    felts.push(...word.toFelts());
  }
  const payload = new FeltArray(felts);
  const configHash = Rpo256.hashElements(payload);
  return { configHash, payload };
}

/**
 * Build the update_signers tx script.
 */
async function buildUpdateSignersScript(webClient: WebClient): Promise<TransactionScript> {
  const multisigMasm = await getMultisigMasm();
  const psmMasm = await getPsmMasm();

  const libBuilder = webClient.createScriptBuilder();

  // Link PSM first (static) - needed by multisig
  const psmLib = libBuilder.buildLibrary('openzeppelin::psm', psmMasm);
  libBuilder.linkStaticLibrary(psmLib);

  // Build and link multisig (dynamic - for FPI on-chain)
  const multisigLib = libBuilder.buildLibrary('auth::multisig', multisigMasm);
  libBuilder.linkDynamicLibrary(multisigLib);

  // Use the module and call with module name (not full path)
  const scriptSource = `
use.auth::multisig

begin
    call.multisig::update_signers_and_threshold
end
  `;

  return libBuilder.compileTxScript(scriptSource);
}

/**
 * Build an update_signers TransactionRequest (no signatures; for summary only).
 */
export async function buildUpdateSignersTransactionRequest(
  webClient: WebClient,
  threshold: number,
  signerCommitments: string[],
  salt?: Word,
): Promise<{ request: TransactionRequest; salt: Word; configHash: Word }> {
  const { configHash, payload } = buildMultisigConfigAdvice(threshold, signerCommitments);
  const advice = new AdviceMap();
  advice.insert(configHash, payload);

  const script = await buildUpdateSignersScript(webClient);
  const authSalt = salt ?? Rpo256.hashElements(new FeltArray([new Felt(BigInt(Date.now()))]));

  const txBuilder = new TransactionRequestBuilder()
    .withCustomScript(script)
    .withScriptArg(configHash)
    .extendAdviceMap(advice)
    .withAuthArg(authSalt);

  return {
    request: txBuilder.build(),
    salt: authSalt,
    configHash,
  };
}

/**
 * Build an update_signers TransactionRequest with signature advice map.
 * This is used for actual execution (not just summary).
 */
export async function buildUpdateSignersTransactionRequestWithSignatures(
  webClient: WebClient,
  threshold: number,
  signerCommitments: string[],
  salt: Word,
  signatureAdviceMap: AdviceMap,
): Promise<TransactionRequest> {
  const { configHash, payload } = buildMultisigConfigAdvice(threshold, signerCommitments);
  const advice = new AdviceMap();
  advice.insert(configHash, payload);

  const script = await buildUpdateSignersScript(webClient);

  const txBuilder = new TransactionRequestBuilder()
    .withCustomScript(script)
    .withScriptArg(configHash)
    .extendAdviceMap(advice)
    .extendAdviceMap(signatureAdviceMap)
    .withAuthArg(salt);

  return txBuilder.build();
}

// =============================================================================
// Signature helpers (for executeProposal)
// =============================================================================

/**
 * Convert hex string to Uint8Array for signature deserialization.
 */
export function hexToUint8Array(hex: string): Uint8Array {
  const cleanHex = hex.startsWith('0x') ? hex.slice(2) : hex;
  const bytes = new Uint8Array(cleanHex.length / 2);
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(cleanHex.substring(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

/**
 * Convert hex signature to bytes with auth scheme prefix for Signature.deserialize().
 *
 * The signatures stored in PSM are raw Falcon signatures without the auth scheme byte.
 * Signature.deserialize() expects the first byte to be the auth scheme identifier
 * (0 = RpoFalcon512). This function prepends that byte.
 */
export function signatureHexToBytes(hex: string): Uint8Array {
  const sigBytes = hexToUint8Array(hex);
  // Prepend auth scheme byte (0 = RpoFalcon512)
  const withPrefix = new Uint8Array(sigBytes.length + 1);
  withPrefix[0] = 0; // RpoFalcon512
  withPrefix.set(sigBytes, 1);
  return withPrefix;
}

/**
 * Build a signature advice entry for the advice map.
 * Key = Hash(pubkey_commitment, message)
 * Value = signature.toPreparedSignature(message)
 *
 * This matches the Rust client behavior in configuration.rs:build_signature_advice_entry
 */
export function buildSignatureAdviceEntry(
  pubkeyCommitment: Word,
  message: Word,
  signature: Signature,
): { key: Word; values: Felt[] } {
  // Merge the two Words using Rpo256.hashElements
  const elements = new FeltArray([
    ...pubkeyCommitment.toFelts(),
    ...message.toFelts(),
  ]);
  const key = Rpo256.hashElements(elements);
  const values = signature.toPreparedSignature(message);
  return { key, values };
}

// =============================================================================
// Transaction builders (update_psm_public_key)
// =============================================================================

/**
 * Build the update_psm_public_key tx script.
 */
async function buildUpdatePsmScript(webClient: WebClient): Promise<TransactionScript> {
  const psmMasm = await getPsmMasm();

  const libBuilder = webClient.createScriptBuilder();

  // Build and link PSM library (dynamic for FPI on-chain)
  const psmLib = libBuilder.buildLibrary('openzeppelin::psm', psmMasm);
  libBuilder.linkDynamicLibrary(psmLib);

  // Script matches the Rust version's logic:
  // 1. adv.push_mapval - pushes value from advice map to advice stack using script_arg as key
  // 2. dropw - clears the key from operand stack
  // 3. call update_psm_public_key - which reads the new key via adv_loadw
  const scriptSource = `
use.openzeppelin::psm

begin
    adv.push_mapval
    dropw
    call.psm::update_psm_public_key
end
  `;

  return libBuilder.compileTxScript(scriptSource);
}

/**
 * Build an update_psm_public_key TransactionRequest (no signatures; for summary only).
 */
export async function buildUpdatePsmTransactionRequest(
  webClient: WebClient,
  newPsmPubkey: string,
  salt?: Word,
): Promise<{ request: TransactionRequest; salt: Word }> {
  const script = await buildUpdatePsmScript(webClient);
  const authSalt = salt ?? Rpo256.hashElements(new FeltArray([new Felt(BigInt(Date.now()))]));

  // The new PSM pubkey is stored in the advice map with itself as the key
  const pubkeyWord = Word.fromHex(normalizeHexWord(newPsmPubkey));
  const advice = new AdviceMap();
  advice.insert(pubkeyWord, new FeltArray(pubkeyWord.toFelts()));

  const txBuilder = new TransactionRequestBuilder()
    .withCustomScript(script)
    .withScriptArg(pubkeyWord)
    .extendAdviceMap(advice)
    .withAuthArg(authSalt);

  return {
    request: txBuilder.build(),
    salt: authSalt,
  };
}

/**
 * Build an update_psm_public_key TransactionRequest with signature advice map.
 * This is used for actual execution.
 */
export async function buildUpdatePsmTransactionRequestWithSignatures(
  webClient: WebClient,
  newPsmPubkey: string,
  salt: Word,
  signatureAdviceMap: AdviceMap,
): Promise<TransactionRequest> {
  const script = await buildUpdatePsmScript(webClient);

  // The new PSM pubkey is stored in the advice map with itself as the key
  const pubkeyWord = Word.fromHex(normalizeHexWord(newPsmPubkey));
  const advice = new AdviceMap();
  advice.insert(pubkeyWord, new FeltArray(pubkeyWord.toFelts()));

  const txBuilder = new TransactionRequestBuilder()
    .withCustomScript(script)
    .withScriptArg(pubkeyWord)
    .extendAdviceMap(advice)
    .extendAdviceMap(signatureAdviceMap)
    .withAuthArg(salt);

  return txBuilder.build();
}

