import {
  Felt,
  FeltArray,
  type MidenClient,
  Poseidon2,
  TransactionRequest,
  TransactionScript,
  Word,
  Word as WordType,
} from '@miden-sdk/miden-sdk';
import { getProcedureRoot, type ProcedureName } from '../procedures.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { buildMultisigRequest, multisigRequestBuilder } from './authArgs.js';
import type { MultisigRequestOptions } from './options.js';

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
  client: MidenClient,
  procedure: ProcedureName,
  threshold: number,
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

  return client.compile.txScript({ code: scriptSource });
}

export async function buildUpdateProcedureThresholdTransactionRequest(
  client: MidenClient,
  procedure: ProcedureName,
  threshold: number,
  options: MultisigRequestOptions,
): Promise<{ request: TransactionRequest; salt: Word; configHash: Word }> {
  const configHash = buildProcedureThresholdConfigHash(procedure, threshold);

  const script = await buildUpdateProcedureThresholdScript(client, procedure, threshold);
  const { builder, saltHex } = await multisigRequestBuilder(client, options);
  let txBuilder = builder.withCustomScript(script);

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
  }

  return { ...buildMultisigRequest(txBuilder, saltHex, options.accountId), configHash };
}
