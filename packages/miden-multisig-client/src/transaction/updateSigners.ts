import {
  AdviceMap,
  Felt,
  FeltArray,
  Rpo256,
  TransactionRequest,
  TransactionRequestBuilder,
  TransactionScript,
  WebClient,
  Word,
  Word as WordType,
} from '@demox-labs/miden-sdk';
import { getMultisigMasm, getPsmMasm } from '../account/masm.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { randomWord } from '../utils/random.js';
import type { SignatureOptions } from './options.js';

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
  const configHash = Rpo256.hashElements(payload);
  return { configHash, payload };
}

async function buildUpdateSignersScript(webClient: WebClient): Promise<TransactionScript> {
  const multisigMasm = await getMultisigMasm();
  const psmMasm = await getPsmMasm();

  const libBuilder = webClient.createScriptBuilder();
  const psmLib = libBuilder.buildLibrary('openzeppelin::psm', psmMasm);
  libBuilder.linkStaticLibrary(psmLib);

  const multisigLib = libBuilder.buildLibrary('auth::multisig', multisigMasm);
  libBuilder.linkDynamicLibrary(multisigLib);

  const scriptSource = `
use.auth::multisig

begin
    call.multisig::update_signers_and_threshold
end
  `;

  return libBuilder.compileTxScript(scriptSource);
}

export async function buildUpdateSignersTransactionRequest(
  webClient: WebClient,
  threshold: number,
  signerCommitments: string[],
  options: SignatureOptions = {},
): Promise<{ request: TransactionRequest; salt: Word; configHash: Word }> {
  // Build config advice - this generates the hash and payload
  const { configHash: configHashForAdvice, payload } = buildMultisigConfigAdvice(threshold, signerCommitments);

  // Create a second config hash for use as script arg (WASM objects get consumed)
  const { configHash: configHashForScript } = buildMultisigConfigAdvice(threshold, signerCommitments);

  // Create a third one to return (will be consumed when caller uses it)
  const { configHash: configHashForReturn } = buildMultisigConfigAdvice(threshold, signerCommitments);

  const advice = new AdviceMap();
  advice.insert(configHashForAdvice, payload);

  const script = await buildUpdateSignersScript(webClient);

  // Store salt as hex so we can create fresh Word instances (WASM objects get consumed)
  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();

  // Create fresh Word for withAuthArg
  const authSaltForBuilder = WordType.fromHex(normalizeHexWord(authSaltHex));

  console.log('[buildUpdateSignersTransactionRequest] Building transaction...');
  console.log('[buildUpdateSignersTransactionRequest] script:', !!script);
  console.log('[buildUpdateSignersTransactionRequest] configHashForScript:', !!configHashForScript);
  console.log('[buildUpdateSignersTransactionRequest] advice:', !!advice);
  console.log('[buildUpdateSignersTransactionRequest] authSaltForBuilder:', !!authSaltForBuilder);
  console.log('[buildUpdateSignersTransactionRequest] options.signatureAdviceMap:', !!options.signatureAdviceMap);

  let txBuilder = new TransactionRequestBuilder();
  console.log('[buildUpdateSignersTransactionRequest] Created builder');

  txBuilder = txBuilder.withCustomScript(script);
  console.log('[buildUpdateSignersTransactionRequest] Added script');

  txBuilder = txBuilder.withScriptArg(configHashForScript);
  console.log('[buildUpdateSignersTransactionRequest] Added script arg');

  txBuilder = txBuilder.extendAdviceMap(advice);
  console.log('[buildUpdateSignersTransactionRequest] Extended advice map (internal)');

  txBuilder = txBuilder.withAuthArg(authSaltForBuilder);
  console.log('[buildUpdateSignersTransactionRequest] Added auth arg');

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
    console.log('[buildUpdateSignersTransactionRequest] Extended advice map (signatures)');
  }

  console.log('[buildUpdateSignersTransactionRequest] About to build...');

  // Create fresh Word for return value
  const authSaltForReturn = WordType.fromHex(normalizeHexWord(authSaltHex));

  const request = txBuilder.build();
  console.log('[buildUpdateSignersTransactionRequest] Build successful');

  return {
    request,
    salt: authSaltForReturn,
    configHash: configHashForReturn,
  };
}

