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
import { GUARDIAN_ECDSA_MASM, GUARDIAN_MASM } from '../account/masm/auth.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { randomWord } from '../utils/random.js';
import type { SignatureOptions } from './options.js';
import type { SignatureScheme } from '../types.js';

function buildUpdateGuardianScript(
  webClient: WebClient,
  signatureScheme: SignatureScheme,
): TransactionScript {
  const libBuilder = webClient.createCodeBuilder();
  const guardianLibraryPath = signatureScheme === 'ecdsa' ? 'openzeppelin::guardian_ecdsa' : 'openzeppelin::guardian';
  const guardianMasm = signatureScheme === 'ecdsa' ? GUARDIAN_ECDSA_MASM : GUARDIAN_MASM;
  const guardianProcedure = signatureScheme === 'ecdsa' ? 'guardian_ecdsa' : 'guardian';
  const guardianLib = libBuilder.buildLibrary(guardianLibraryPath, guardianMasm);
  libBuilder.linkDynamicLibrary(guardianLib);

  const scriptSource = `
use openzeppelin::${guardianProcedure}

begin
    adv.push_mapval
    dropw
    call.${guardianProcedure}::update_guardian_public_key
end
  `;

  return libBuilder.compileTxScript(scriptSource);
}

export async function buildUpdateGuardianTransactionRequest(
  webClient: WebClient,
  newGuardianPubkey: string,
  options: SignatureOptions = {},
): Promise<{ request: TransactionRequest; salt: Word }> {
  const signatureScheme = options.signatureScheme ?? 'falcon';
  const script = buildUpdateGuardianScript(webClient, signatureScheme);

  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();

  const pubkeyWordForAdvice = WordType.fromHex(normalizeHexWord(newGuardianPubkey));
  const pubkeyWordForFelts = WordType.fromHex(normalizeHexWord(newGuardianPubkey));
  const pubkeyWordForScript = WordType.fromHex(normalizeHexWord(newGuardianPubkey));

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
