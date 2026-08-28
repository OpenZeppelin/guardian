import {
  AdviceMap,
  Felt,
  FeltArray,
  type MidenClient,
  Poseidon2,
  TransactionRequest,
  TransactionRequestBuilder,
  TransactionScript,
  type WasmWebClient,
  Word,
  Word as WordType,
} from '@miden-sdk/miden-sdk';
import { compileTxScript } from '../raw-client.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { resolveAuthArg } from './feeAuth.js';
import { randomWord } from '../utils/random.js';
import { authSchemeId } from '../utils/signature.js';
import type { MidenClientSignatureOptions, SignatureOptions } from './options.js';
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

async function buildUpdateSignersScript(
  client: MidenClient | WasmWebClient,
  midenRpcEndpoint?: string,
): Promise<TransactionScript> {
  const scriptSource = `
use miden::standards::auth::multisig

@transaction_script
pub proc main
    call.multisig::update_signers_and_threshold
end
  `;

  return compileTxScript(client, scriptSource, [], midenRpcEndpoint);
}

export function buildUpdateSignersTransactionRequest(
  client: MidenClient,
  threshold: number,
  signerCommitments: string[],
  options: MidenClientSignatureOptions,
): Promise<{ request: TransactionRequest; salt: Word; configHash: Word }>;
export function buildUpdateSignersTransactionRequest(
  client: WasmWebClient,
  threshold: number,
  signerCommitments: string[],
  options?: SignatureOptions,
): Promise<{ request: TransactionRequest; salt: Word; configHash: Word }>;
export async function buildUpdateSignersTransactionRequest(
  client: MidenClient | WasmWebClient,
  threshold: number,
  signerCommitments: string[],
  options: SignatureOptions = {},
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

  const script = await buildUpdateSignersScript(client, options.midenRpcEndpoint);

  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();

  const authSaltForBuilder = WordType.fromHex(normalizeHexWord(authSaltHex));

  let txBuilder = new TransactionRequestBuilder();
  txBuilder = txBuilder.withCustomScript(script);
  txBuilder = txBuilder.withScriptArg(configHashForScript);
  txBuilder = txBuilder.extendAdviceMap(advice);
  const feeAuth = resolveAuthArg(authSaltForBuilder, options.feeFaucetId);
  txBuilder = txBuilder.withAuthArg(feeAuth.authArg);
  if (feeAuth.adviceMap) {
    txBuilder = txBuilder.extendAdviceMap(feeAuth.adviceMap);
  }

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
  }

  const authSaltForReturn = WordType.fromHex(normalizeHexWord(authSaltHex));

  return {
    request: txBuilder.build(),
    salt: authSaltForReturn,
    configHash: configHashForReturn,
  };
}
