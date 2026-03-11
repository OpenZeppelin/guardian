import {
  Felt,
  FeltArray,
  Rpo256,
  TransactionRequest,
  TransactionRequestBuilder,
  TransactionScript,
  WebClient,
  Word,
  Word as WordType,
} from '@miden-sdk/miden-sdk';
import { MULTISIG_MASM, PSM_MASM } from '../account/masm.js';
import { getProcedureRoot, type ProcedureName } from '../procedures.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { randomWord } from '../utils/random.js';
import type { SignatureOptions } from './options.js';

function buildProcedureThresholdAdvice(
  procedure: ProcedureName,
  threshold: number,
): { configHash: Word; payload: FeltArray } {
  const procedureRoot = WordType.fromHex(normalizeHexWord(getProcedureRoot(procedure)));
  const payload = new FeltArray([
    ...procedureRoot.toFelts(),
    new Felt(BigInt(threshold)),
    new Felt(0n),
    new Felt(0n),
    new Felt(0n),
  ]);
  const configHash = Rpo256.hashElements(payload);
  return { configHash, payload };
}

function buildUpdateProcedureThresholdScript(
  webClient: WebClient,
  procedure: ProcedureName,
  threshold: number,
): TransactionScript {
  const libBuilder = webClient.createCodeBuilder();
  const psmLib = libBuilder.buildLibrary('openzeppelin::psm', PSM_MASM);
  libBuilder.linkStaticLibrary(psmLib);

  const multisigLib = libBuilder.buildLibrary('auth::multisig', MULTISIG_MASM);
  libBuilder.linkDynamicLibrary(multisigLib);
  const procedureRoot = normalizeHexWord(getProcedureRoot(procedure));

  const scriptSource = `
use auth::multisig

begin
    push.${procedureRoot}
    push.${threshold}
    call.multisig::update_procedure_threshold
    dropw
    drop
end
  `;

  return libBuilder.compileTxScript(scriptSource);
}

export async function buildUpdateProcedureThresholdTransactionRequest(
  webClient: WebClient,
  procedure: ProcedureName,
  threshold: number,
  options: SignatureOptions = {},
): Promise<{ request: TransactionRequest; salt: Word; configHash: Word }> {
  const { configHash } = buildProcedureThresholdAdvice(procedure, threshold);

  const script = buildUpdateProcedureThresholdScript(webClient, procedure, threshold);
  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();
  const authSalt = WordType.fromHex(normalizeHexWord(authSaltHex));

  let txBuilder = new TransactionRequestBuilder();
  txBuilder = txBuilder.withCustomScript(script);
  txBuilder = txBuilder.withAuthArg(authSalt);

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
  }

  return {
    request: txBuilder.build(),
    salt: WordType.fromHex(normalizeHexWord(authSaltHex)),
    configHash,
  };
}
