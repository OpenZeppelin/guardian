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
import { expirationInstructions } from './expiration.js';
import type { MultisigRequestOptions } from './options.js';
import type { SignatureScheme } from '../types.js';

function buildMultisigConfigFelts(
  threshold: number,
  signerCommitments: string[],
  signatureScheme: SignatureScheme,
): Felt[] {
  const numApprovers = signerCommitments.length;
  const schemeId = authSchemeId(signatureScheme);
  const felts: Felt[] = [
    new Felt(BigInt(threshold)),
    new Felt(BigInt(numApprovers)),
    new Felt(0n),
    new Felt(0n),
  ];
  // Interleave [PUB_KEY, SCHEME_ID] per approver, in reverse index order.
  for (const commitment of [...signerCommitments].reverse()) {
    const word = WordType.fromHex(normalizeHexWord(commitment));
    felts.push(...word.toFelts());
    felts.push(new Felt(BigInt(schemeId)), new Felt(0n), new Felt(0n), new Felt(0n));
  }
  return felts;
}

export function buildMultisigConfigAdvice(
  threshold: number,
  signerCommitments: string[],
  signatureScheme: SignatureScheme,
): { configHash: Word; payload: FeltArray } {
  // `Poseidon2.hashElements` consumes (frees) its `FeltArray` by value, so the advice payload
  // must be a separately built array — reusing the hashed array surfaces as "null pointer
  // passed to rust" at the later `advice.insert`.
  const configHash = Poseidon2.hashElements(
    new FeltArray(buildMultisigConfigFelts(threshold, signerCommitments, signatureScheme)),
  );
  const payload = new FeltArray(
    buildMultisigConfigFelts(threshold, signerCommitments, signatureScheme),
  );
  return { configHash, payload };
}

export async function buildUpdateSignersScript(
  client: MidenClient,
  transactionExpirationDelta: number | undefined,
): Promise<TransactionScript> {
  const scriptSource = `
use miden::standards::auth::multisig

@transaction_script
pub proc main
    ${expirationInstructions(transactionExpirationDelta)}call.multisig::update_signers_and_threshold
end
  `;

  return client.compile.txScript({ code: scriptSource });
}

export async function buildUpdateSignersTransactionRequest(
  client: MidenClient,
  threshold: number,
  signerCommitments: string[],
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

  const script = await buildUpdateSignersScript(client, options.transactionExpirationDelta);

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
