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
  // Must be a word literal, not a dotted felt list: `push.<word>` pushes in
  // reverse so Word[0] lands on top, while `push.f0.f1.f2.f3` leaves Word[3] on
  // top. The component stores the four elements verbatim, so the dotted form
  // writes the key reversed and diverges from the Rust builder.
  const keyLiteral = normalizeHexWord(newGuardianPubkey);
  const schemeId = authSchemeId(signatureScheme);

  // The Rust builder calls the guarded-multisig component's re-export
  // (`::miden::standards::components::auth::guarded_multisig::update_guardian_public_key`);
  // the web SDK assembler links only the `miden::standards` library, so this calls the
  // origin procedure directly — a re-export shares its MAST root, so the compiled call
  // is identical.
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
