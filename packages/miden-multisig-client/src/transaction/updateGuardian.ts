import {
  type MidenClient,
  TransactionRequest,
  TransactionScript,
  type WasmWebClient,
  Word,
} from '@miden-sdk/miden-sdk';
import { compileTxScript } from '../raw-client.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { authSchemeId } from '../utils/signature.js';
import { buildMultisigRequest, multisigRequestBuilder } from './authArgs.js';
import type { MidenClientMultisigRequestOptions, MultisigRequestOptions } from './options.js';
import type { SignatureScheme } from '../types.js';

async function buildUpdateGuardianScript(
  client: MidenClient | WasmWebClient,
  newGuardianPubkey: string,
  signatureScheme: SignatureScheme,
  midenRpcEndpoint?: string,
): Promise<TransactionScript> {
  // A word literal preserves the key's element order on the operand stack.
  const keyLiteral = normalizeHexWord(newGuardianPubkey);
  const schemeId = authSchemeId(signatureScheme);

  // Calling the origin procedure yields the same MAST root as its component re-export.
  const scriptSource = `
use miden::standards::auth::guardian

@transaction_script
pub proc main
    push.${keyLiteral}
    push.${schemeId}
    call.guardian::update_guardian_public_key
    drop
    dropw
end
  `;

  return compileTxScript(client, scriptSource, [], midenRpcEndpoint);
}

export function buildUpdateGuardianTransactionRequest(
  client: MidenClient,
  newGuardianPubkey: string,
  options: MidenClientMultisigRequestOptions,
): Promise<{ request: TransactionRequest; salt: Word }>;
export function buildUpdateGuardianTransactionRequest(
  client: WasmWebClient,
  newGuardianPubkey: string,
  options: MultisigRequestOptions,
): Promise<{ request: TransactionRequest; salt: Word }>;
export async function buildUpdateGuardianTransactionRequest(
  client: MidenClient | WasmWebClient,
  newGuardianPubkey: string,
  options: MultisigRequestOptions,
): Promise<{ request: TransactionRequest; salt: Word }> {
  const signatureScheme = options.signatureScheme ?? 'falcon';
  const script = await buildUpdateGuardianScript(
    client,
    newGuardianPubkey,
    signatureScheme,
    options.midenRpcEndpoint,
  );

  const { builder, saltHex } = await multisigRequestBuilder(client, options);
  let txBuilder = builder.withCustomScript(script);

  if (options.signatureAdviceMap) {
    txBuilder = txBuilder.extendAdviceMap(options.signatureAdviceMap);
  }

  return buildMultisigRequest(txBuilder, saltHex, options.accountId);
}
