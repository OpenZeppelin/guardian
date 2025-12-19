import {
  AdviceMap,
  FeltArray,
  TransactionRequest,
  TransactionRequestBuilder,
  TransactionScript,
  WebClient,
  Word,
  Word as WordType,
} from '@demox-labs/miden-sdk';
import { getPsmMasm } from '../account/masm.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { randomWord } from '../utils/random.js';
import type { SignatureOptions } from './options.js';

async function buildUpdatePsmScript(webClient: WebClient): Promise<TransactionScript> {
  const psmMasm = await getPsmMasm();
  const libBuilder = webClient.createScriptBuilder();
  const psmLib = libBuilder.buildLibrary('openzeppelin::psm', psmMasm);
  libBuilder.linkDynamicLibrary(psmLib);

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

export async function buildUpdatePsmTransactionRequest(
  webClient: WebClient,
  newPsmPubkey: string,
  options: SignatureOptions = {},
): Promise<{ request: TransactionRequest; salt: Word }> {
  const script = await buildUpdatePsmScript(webClient);

  // Store salt as hex so we can create fresh Word instances (WASM objects get consumed)
  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();

  // Create separate Word instances for each use (WASM objects get consumed)
  const pubkeyWordForAdvice = WordType.fromHex(normalizeHexWord(newPsmPubkey));
  const pubkeyWordForFelts = WordType.fromHex(normalizeHexWord(newPsmPubkey));
  const pubkeyWordForScript = WordType.fromHex(normalizeHexWord(newPsmPubkey));

  const advice = new AdviceMap();
  advice.insert(pubkeyWordForAdvice, new FeltArray(pubkeyWordForFelts.toFelts()));

  // Create fresh Word for withAuthArg
  const authSaltForBuilder = WordType.fromHex(normalizeHexWord(authSaltHex));

  console.log('[buildUpdatePsmTransactionRequest] Building transaction...');
  console.log('[buildUpdatePsmTransactionRequest] newPsmPubkey:', newPsmPubkey);
  console.log('[buildUpdatePsmTransactionRequest] options.signatureAdviceMap:', !!options.signatureAdviceMap);

  let txBuilder = new TransactionRequestBuilder();
  console.log('[buildUpdatePsmTransactionRequest] Created builder');

  txBuilder = txBuilder.withCustomScript(script);
  console.log('[buildUpdatePsmTransactionRequest] Added script');

  txBuilder = txBuilder.withScriptArg(pubkeyWordForScript);
  console.log('[buildUpdatePsmTransactionRequest] Added script arg');

  txBuilder = txBuilder.extendAdviceMap(advice);
  console.log('[buildUpdatePsmTransactionRequest] Extended advice map (internal)');

  txBuilder = txBuilder.withAuthArg(authSaltForBuilder);
  console.log('[buildUpdatePsmTransactionRequest] Added auth arg');

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
    console.log('[buildUpdatePsmTransactionRequest] Extended advice map (signatures)');
  }

  console.log('[buildUpdatePsmTransactionRequest] About to build...');

  // Create fresh Word for return value
  const authSaltForReturn = WordType.fromHex(normalizeHexWord(authSaltHex));

  const request = txBuilder.build();
  console.log('[buildUpdatePsmTransactionRequest] Build successful');

  return {
    request,
    salt: authSaltForReturn,
  };
}

