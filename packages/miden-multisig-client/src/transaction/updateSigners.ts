import {
  AdviceMap,
  Felt,
  FeltArray,
  type MidenClient,
  Poseidon2,
  TransactionRequest,
  TransactionScript,
  Word,
  Word as WordType,
} from '@miden-sdk/miden-sdk';
import { normalizeHexWord } from '../utils/encoding.js';
import { authSchemeId } from '../utils/signature.js';
import { buildMultisigRequest, multisigRequestBuilder } from './authArgs.js';
import type { MultisigRequestOptions } from './options.js';
import type { SignatureScheme, SignerInput, SignerSpec } from '../types.js';
import { resolveSignerSpecs } from '../account/signers.js';

/**
 * The `update_signers_and_threshold` advice payload:
 * `[CONFIG, PUB_KEY_N, SCHEME_ID_N, ..., PUB_KEY_0, SCHEME_ID_0]`, one scheme per approver.
 */
function buildMultisigConfigFelts(threshold: number, signers: readonly SignerSpec[]): Felt[] {
  const felts: Felt[] = [
    new Felt(BigInt(threshold)),
    new Felt(BigInt(signers.length)),
    new Felt(0n),
    new Felt(0n),
  ];
  for (const { commitment, scheme } of [...signers].reverse()) {
    const word = WordType.fromHex(normalizeHexWord(commitment));
    felts.push(...word.toFelts());
    felts.push(new Felt(BigInt(authSchemeId(scheme))), new Felt(0n), new Felt(0n), new Felt(0n));
  }
  return felts;
}

/**
 * Builds the new-config advice for `update_signers_and_threshold`.
 *
 * @param signers - The new approver set in storage order; a bare commitment takes
 *   `signatureScheme`, a {@link SignerSpec} keeps its own scheme
 * @param signatureScheme - Scheme for approvers given as bare commitments
 */
export function buildMultisigConfigAdvice(
  threshold: number,
  signers: readonly SignerInput[],
  signatureScheme: SignatureScheme,
): { configHash: Word; payload: FeltArray } {
  const specs = resolveSignerSpecs({ signerCommitments: [...signers], signatureScheme });
  // `Poseidon2.hashElements` consumes (frees) its `FeltArray` by value, so the advice payload
  // must be a separately built array — reusing the hashed array surfaces as "null pointer
  // passed to rust" at the later `advice.insert`.
  const configHash = Poseidon2.hashElements(
    new FeltArray(buildMultisigConfigFelts(threshold, specs)),
  );
  const payload = new FeltArray(buildMultisigConfigFelts(threshold, specs));
  return { configHash, payload };
}

async function buildUpdateSignersScript(client: MidenClient): Promise<TransactionScript> {
  const scriptSource = `
use miden::standards::auth::multisig

@transaction_script
pub proc main
    call.multisig::update_signers_and_threshold
end
  `;

  return client.compile.txScript({ code: scriptSource });
}

export async function buildUpdateSignersTransactionRequest(
  client: MidenClient,
  threshold: number,
  signerCommitments: readonly SignerInput[],
  options: MultisigRequestOptions,
): Promise<{ request: TransactionRequest; salt: Word; configHash: Word }> {
  const signatureScheme = options.signatureScheme ?? 'falcon';
  const { configHash: configHashForAdvice, payload } = buildMultisigConfigAdvice(
    threshold,
    signerCommitments,
    signatureScheme,
  );

  const { configHash: configHashForScript } = buildMultisigConfigAdvice(
    threshold,
    signerCommitments,
    signatureScheme,
  );

  const { configHash: configHashForReturn } = buildMultisigConfigAdvice(
    threshold,
    signerCommitments,
    signatureScheme,
  );

  const advice = new AdviceMap();
  advice.insert(configHashForAdvice, payload);

  const script = await buildUpdateSignersScript(client);

  const { builder, saltHex } = await multisigRequestBuilder(client, options);
  let txBuilder = builder
    .withCustomScript(script)
    .withScriptArg(configHashForScript)
    .extendAdviceMap(advice);

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
  }

  return {
    ...buildMultisigRequest(txBuilder, saltHex, options.accountId),
    configHash: configHashForReturn,
  };
}
