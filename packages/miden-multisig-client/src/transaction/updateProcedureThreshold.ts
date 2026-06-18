import {
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
import { getProcedureRoot, type ProcedureName } from '../procedures.js';
import { compileTxScript } from '../raw-client.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { randomWord } from '../utils/random.js';
import type { SignatureOptions } from './options.js';
import type { SignatureScheme } from '../types.js';

function buildProcedureThresholdFelts(procedure: ProcedureName, threshold: number): Felt[] {
  const procedureRoot = WordType.fromHex(normalizeHexWord(getProcedureRoot(procedure)));
  return [
    ...procedureRoot.toFelts(),
    new Felt(BigInt(threshold)),
    new Felt(0n),
    new Felt(0n),
    new Felt(0n),
  ];
}

function buildProcedureThresholdAdvice(
  procedure: ProcedureName,
  threshold: number,
): { configHash: Word; payload: FeltArray } {
  // `Poseidon2.hashElements` takes its `FeltArray` by value and frees the JS
  // wrapper (wasm-bindgen). Hash one FeltArray and return a separate, freshly
  // built one as the advice payload — reusing the hashed FeltArray surfaces as
  // "null pointer passed to rust" at the later `advice.insert`.
  const configHash = Poseidon2.hashElements(
    new FeltArray(buildProcedureThresholdFelts(procedure, threshold)),
  );
  const payload = new FeltArray(buildProcedureThresholdFelts(procedure, threshold));
  return { configHash, payload };
}

async function buildUpdateProcedureThresholdScript(
  client: MidenClient | WasmWebClient,
  procedure: ProcedureName,
  threshold: number,
  signatureScheme: SignatureScheme,
  midenRpcEndpoint?: string,
): Promise<TransactionScript> {
  const multisigMasm = signatureScheme === 'ecdsa' ? MULTISIG_ECDSA_MASM : MULTISIG_MASM;
  const procedureRoot = normalizeHexWord(getProcedureRoot(procedure));

  const scriptSource = `
use oz_multisig::multisig

begin
    push.${procedureRoot}
    push.${threshold}
    call.multisig::update_procedure_threshold
    dropw
    drop
end
  `;

  return compileTxScript(
    client,
    scriptSource,
    [{ namespace: 'oz_multisig::multisig', code: multisigMasm }],
    midenRpcEndpoint,
  );
}

export async function buildUpdateProcedureThresholdTransactionRequest(
  client: MidenClient | WasmWebClient,
  procedure: ProcedureName,
  threshold: number,
  options: SignatureOptions = {},
): Promise<{ request: TransactionRequest; salt: Word; configHash: Word }> {
  const signatureScheme = options.signatureScheme ?? 'falcon';
  const { configHash } = buildProcedureThresholdAdvice(procedure, threshold);

  const script = await buildUpdateProcedureThresholdScript(
    client,
    procedure,
    threshold,
    signatureScheme,
    options.midenRpcEndpoint,
  );
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
