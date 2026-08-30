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
import { getProcedureRoot, type ProcedureName } from '../procedures.js';
import { compileTxScript } from '../raw-client.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { applyAuthArg } from './feeAuth.js';
import { randomWord } from '../utils/random.js';
import type { MidenClientSignatureOptions, SignatureOptions } from './options.js';

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

/**
 * `set_procedure_threshold` reads its `[proc_threshold, PROC_ROOT]` inputs from the operand stack
 * (pushed by the script), so no advice-map entry is attached; this hash is returned only for
 * caller bookkeeping.
 */
function buildProcedureThresholdConfigHash(procedure: ProcedureName, threshold: number): Word {
  return Poseidon2.hashElements(
    new FeltArray(buildProcedureThresholdFelts(procedure, threshold)),
  );
}

async function buildUpdateProcedureThresholdScript(
  client: MidenClient | WasmWebClient,
  procedure: ProcedureName,
  threshold: number,
  midenRpcEndpoint?: string,
): Promise<TransactionScript> {
  const procedureRoot = normalizeHexWord(getProcedureRoot(procedure));

  const scriptSource = `
use miden::standards::auth::multisig

@transaction_script
pub proc main
    push.${procedureRoot}
    push.${threshold}
    call.multisig::set_procedure_threshold
    dropw
    drop
end
  `;

  return compileTxScript(client, scriptSource, [], midenRpcEndpoint);
}

export function buildUpdateProcedureThresholdTransactionRequest(
  client: MidenClient,
  procedure: ProcedureName,
  threshold: number,
  options: MidenClientSignatureOptions,
): Promise<{ request: TransactionRequest; salt: Word; configHash: Word }>;
export function buildUpdateProcedureThresholdTransactionRequest(
  client: WasmWebClient,
  procedure: ProcedureName,
  threshold: number,
  options?: SignatureOptions,
): Promise<{ request: TransactionRequest; salt: Word; configHash: Word }>;
export async function buildUpdateProcedureThresholdTransactionRequest(
  client: MidenClient | WasmWebClient,
  procedure: ProcedureName,
  threshold: number,
  options: SignatureOptions = {},
): Promise<{ request: TransactionRequest; salt: Word; configHash: Word }> {
  const configHash = buildProcedureThresholdConfigHash(procedure, threshold);

  const script = await buildUpdateProcedureThresholdScript(
    client,
    procedure,
    threshold,
    options.midenRpcEndpoint,
  );
  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();
  const authSalt = WordType.fromHex(normalizeHexWord(authSaltHex));

  let txBuilder = new TransactionRequestBuilder();
  txBuilder = txBuilder.withCustomScript(script);
  txBuilder = applyAuthArg(txBuilder, authSalt, options.feeFaucetId);

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
  }

  return {
    request: txBuilder.build(),
    salt: WordType.fromHex(normalizeHexWord(authSaltHex)),
    configHash,
  };
}
