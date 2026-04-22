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
import {
  MULTISIG_ECDSA_MASM,
  MULTISIG_MASM,
} from '../account/masm/auth.js';
import { compileTxScript } from '../raw-client.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { randomWord } from '../utils/random.js';
import type { SignatureOptions } from './options.js';
import type { SignatureScheme } from '../types.js';

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
  for (const commitment of [...signerCommitments].reverse()) {
    const word = WordType.fromHex(normalizeHexWord(commitment));
    felts.push(...word.toFelts());
  }
  const payload = new FeltArray(felts);
  const configHash = Poseidon2.hashElements(payload);
  return { configHash, payload };
}

async function buildUpdateSignersScript(
  client: MidenClient | WasmWebClient,
  signatureScheme: SignatureScheme,
  midenRpcEndpoint?: string,
): Promise<TransactionScript> {
  const multisigMasm = signatureScheme === 'ecdsa' ? MULTISIG_ECDSA_MASM : MULTISIG_MASM;

  const scriptSource = `
use oz_multisig::multisig

begin
    call.multisig::update_signers_and_threshold
end
  `;

  return compileTxScript(
    client,
    scriptSource,
    [{ namespace: 'oz_multisig::multisig', code: multisigMasm }],
    midenRpcEndpoint,
  );
}

export async function buildUpdateSignersTransactionRequest(
  client: MidenClient | WasmWebClient,
  threshold: number,
  signerCommitments: string[],
  options: SignatureOptions = {},
): Promise<{ request: TransactionRequest; salt: Word; configHash: Word }> {
  const signatureScheme = options.signatureScheme ?? 'falcon';
  const { configHash: configHashForAdvice, payload } = buildMultisigConfigAdvice(threshold, signerCommitments);

  const { configHash: configHashForScript } = buildMultisigConfigAdvice(threshold, signerCommitments);

  const { configHash: configHashForReturn } = buildMultisigConfigAdvice(threshold, signerCommitments);

  const advice = new AdviceMap();
  advice.insert(configHashForAdvice, payload);

  const script = await buildUpdateSignersScript(
    client,
    signatureScheme,
    options.midenRpcEndpoint,
  );

  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();

  const authSaltForBuilder = WordType.fromHex(normalizeHexWord(authSaltHex));

  let txBuilder = new TransactionRequestBuilder();
  txBuilder = txBuilder.withCustomScript(script);
  txBuilder = txBuilder.withScriptArg(configHashForScript);
  txBuilder = txBuilder.extendAdviceMap(advice);
  txBuilder = txBuilder.withAuthArg(authSaltForBuilder);

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
