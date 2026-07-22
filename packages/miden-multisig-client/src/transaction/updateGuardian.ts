import {
  type MidenClient,
  TransactionRequest,
  TransactionRequestBuilder,
  TransactionScript,
  type WasmWebClient,
  Word,
  Word as WordType,
} from '@miden-sdk/miden-sdk';
import { compileTxScript } from '../raw-client.js';
import { normalizeHexWord } from '../utils/encoding.js';
import { randomWord } from '../utils/random.js';
import { authSchemeId } from '../utils/signature.js';
import type { MidenClientSignatureOptions, SignatureOptions } from './options.js';
import type { SignatureScheme } from '../types.js';

async function buildUpdateGuardianScript(
  client: MidenClient | WasmWebClient,
  newGuardianPubkey: string,
  signatureScheme: SignatureScheme,
  midenRpcEndpoint?: string,
): Promise<TransactionScript> {
  const keyFelts = WordType.fromHex(normalizeHexWord(newGuardianPubkey)).toFelts();
  const keyLiteral = keyFelts.map((felt) => felt.asInt().toString()).join('.');
  const schemeId = authSchemeId(signatureScheme);

  const scriptSource = `
use miden::standards::auth::guardian

begin
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
  options: MidenClientSignatureOptions,
): Promise<{ request: TransactionRequest; salt: Word }>;
export function buildUpdateGuardianTransactionRequest(
  client: WasmWebClient,
  newGuardianPubkey: string,
  options?: SignatureOptions,
): Promise<{ request: TransactionRequest; salt: Word }>;
export async function buildUpdateGuardianTransactionRequest(
  client: MidenClient | WasmWebClient,
  newGuardianPubkey: string,
  options: SignatureOptions = {},
): Promise<{ request: TransactionRequest; salt: Word }> {
  const signatureScheme = options.signatureScheme ?? 'falcon';
  const script = await buildUpdateGuardianScript(
    client,
    newGuardianPubkey,
    signatureScheme,
    options.midenRpcEndpoint,
  );

  const authSaltHex = options.salt ? options.salt.toHex() : randomWord().toHex();
  const authSaltForBuilder = WordType.fromHex(normalizeHexWord(authSaltHex));

  let txBuilder = new TransactionRequestBuilder();
  txBuilder = txBuilder.withCustomScript(script);
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
