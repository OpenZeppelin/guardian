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
import { MULTISIG_ECDSA_MASM, MULTISIG_MASM, PSM_ECDSA_MASM, PSM_MASM } from '../account/masm.js';
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

function buildUpdateSignersFalconScript(webClient: WebClient): TransactionScript {
  const libBuilder = webClient.createScriptBuilder();
  const psmLib = libBuilder.buildLibrary('openzeppelin::psm', PSM_MASM);
  libBuilder.linkStaticLibrary(psmLib);

  const multisigLib = libBuilder.buildLibrary('auth::multisig', MULTISIG_MASM);
  libBuilder.linkDynamicLibrary(multisigLib);

  const scriptSource = `
use.auth::multisig

begin
    call.multisig::update_signers_and_threshold
end
  `;

  return libBuilder.compileTxScript(scriptSource);
}

function buildUpdateSignersEcdsaScript(webClient: WebClient): TransactionScript {
  const libBuilder = webClient.createScriptBuilder();
  const psmLib = libBuilder.buildLibrary('openzeppelin::psm_ecdsa', PSM_ECDSA_MASM);
  libBuilder.linkStaticLibrary(psmLib);

  const multisigLib = libBuilder.buildLibrary('auth::multisig', MULTISIG_ECDSA_MASM);
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
  const signatureScheme = options.signatureScheme ?? 'falcon';
  const { configHash: configHashForAdvice, payload } = buildMultisigConfigAdvice(threshold, signerCommitments);

  const { configHash: configHashForScript } = buildMultisigConfigAdvice(threshold, signerCommitments);

  const { configHash: configHashForReturn } = buildMultisigConfigAdvice(threshold, signerCommitments);

  const advice = new AdviceMap();
  advice.insert(configHashForAdvice, payload);

  const script = signatureScheme === 'ecdsa'
    ? buildUpdateSignersEcdsaScript(webClient)
    : buildUpdateSignersFalconScript(webClient);

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

