import {
  AdviceMap,
  FeltArray,
  TransactionRequest,
  TransactionRequestBuilder,
  TransactionScript,
  WebClient,
  Word,
  Word as WordType,
} from '@miden-sdk/miden-sdk';
import { PSM_ECDSA_MASM, PSM_MASM } from '../account/masm/auth.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { randomWord } from '../utils/random.js';
import type { SignatureOptions } from './options.js';
import type { SignatureScheme } from '../types.js';

function buildUpdatePsmScript(
  webClient: WebClient,
  signatureScheme: SignatureScheme,
): TransactionScript {
  const libBuilder = webClient.createCodeBuilder();
  const psmLibraryPath = signatureScheme === 'ecdsa' ? 'openzeppelin::psm_ecdsa' : 'openzeppelin::psm';
  const psmMasm = signatureScheme === 'ecdsa' ? PSM_ECDSA_MASM : PSM_MASM;
  const psmProcedure = signatureScheme === 'ecdsa' ? 'psm_ecdsa' : 'psm';
  const psmLib = libBuilder.buildLibrary(psmLibraryPath, psmMasm);
  libBuilder.linkDynamicLibrary(psmLib);

  const scriptSource = `
use openzeppelin::${psmProcedure}

begin
    adv.push_mapval
    dropw
    call.${psmProcedure}::update_psm_public_key
end
  `;

  return libBuilder.compileTxScript(scriptSource);
}

export async function buildUpdatePsmTransactionRequest(
  webClient: WebClient,
  newPsmPubkey: string,
  options: SignatureOptions = {},
): Promise<{ request: TransactionRequest; salt: Word }> {
  const signatureScheme = options.signatureScheme ?? 'falcon';
  const script = buildUpdatePsmScript(webClient, signatureScheme);

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
