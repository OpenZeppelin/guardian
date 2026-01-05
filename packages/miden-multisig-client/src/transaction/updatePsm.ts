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

  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();

  const pubkeyWordForAdvice = WordType.fromHex(normalizeHexWord(newPsmPubkey));
  const pubkeyWordForFelts = WordType.fromHex(normalizeHexWord(newPsmPubkey));
  const pubkeyWordForScript = WordType.fromHex(normalizeHexWord(newPsmPubkey));

  const advice = new AdviceMap();
  advice.insert(pubkeyWordForAdvice, new FeltArray(pubkeyWordForFelts.toFelts()));

  const authSaltForBuilder = WordType.fromHex(normalizeHexWord(authSaltHex));

  let txBuilder = new TransactionRequestBuilder();
  txBuilder = txBuilder.withCustomScript(script);
  txBuilder = txBuilder.withScriptArg(pubkeyWordForScript);
  txBuilder = txBuilder.extendAdviceMap(advice);
  txBuilder = txBuilder.withAuthArg(authSaltForBuilder);

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
  }

  const authSaltForReturn = WordType.fromHex(normalizeHexWord(authSaltHex));

  return {
    request: txBuilder.build(),
    salt: authSaltForReturn,
  };
}

