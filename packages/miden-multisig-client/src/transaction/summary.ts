import type {
  MidenClient,
  TransactionRequest,
  TransactionSummary,
  WasmWebClient,
} from '@miden-sdk/miden-sdk';
import { AccountId, ChainAnchor, Word } from '@miden-sdk/miden-sdk';
import { getRawMidenClient } from '../raw-client.js';
import { base64ToUint8Array, uint8ArrayToBase64 } from '../utils/encoding.js';

/**
 * Index of the first user param carrying the auth args. The guarded-multisig
 * auth component zeroes user params 0-2 and fills 3-6 with the auth args, matching
 * `push.0.0.0` ahead of `multisig::auth_tx` in `guarded_multisig.masm`.
 */
const AUTH_ARG_USER_PARAM_OFFSET = 3;

/**
 * Captures a `ChainAnchor` for the request at the current sync height and
 * executes the transaction against it to obtain the summary awaiting
 * authorization. The anchor is returned alongside the summary so the proposer
 * can ship it with the signed data; cosigners and the executor then reproduce
 * the summary — which binds the reference block commitment since protocol
 * 0.16 — with {@link executeForSummaryAt} regardless of their own sync height.
 */
export function executeForSummary(
  client: MidenClient,
  accountId: string,
  txRequest: TransactionRequest,
  midenRpcEndpoint: string,
): Promise<{ summary: TransactionSummary; anchor: ChainAnchor }>;
export function executeForSummary(
  client: WasmWebClient,
  accountId: string,
  txRequest: TransactionRequest,
  midenRpcEndpoint?: string,
): Promise<{ summary: TransactionSummary; anchor: ChainAnchor }>;
export async function executeForSummary(
  client: MidenClient | WasmWebClient,
  accountId: string,
  txRequest: TransactionRequest,
  midenRpcEndpoint?: string,
): Promise<{ summary: TransactionSummary; anchor: ChainAnchor }> {
  const acc = AccountId.fromHex(accountId);
  const rawClient = await getRawMidenClient(client, midenRpcEndpoint);
  const anchor = await rawClient.chainAnchorForRequest(txRequest);
  const summary = await rawClient.executeForSummaryAt(acc, txRequest, anchor);
  return { summary, anchor };
}

/**
 * Executes a transaction at the given `ChainAnchor`'s reference block to
 * obtain the summary awaiting authorization — the anchored counterpart of
 * {@link executeForSummary} for cosigners and executors holding a proposal's
 * anchor.
 */
export function executeForSummaryAt(
  client: MidenClient,
  accountId: string,
  txRequest: TransactionRequest,
  anchor: ChainAnchor,
  midenRpcEndpoint: string,
): Promise<TransactionSummary>;
export function executeForSummaryAt(
  client: WasmWebClient,
  accountId: string,
  txRequest: TransactionRequest,
  anchor: ChainAnchor,
  midenRpcEndpoint?: string,
): Promise<TransactionSummary>;
export async function executeForSummaryAt(
  client: MidenClient | WasmWebClient,
  accountId: string,
  txRequest: TransactionRequest,
  anchor: ChainAnchor,
  midenRpcEndpoint?: string,
): Promise<TransactionSummary> {
  const acc = AccountId.fromHex(accountId);
  const rawClient = await getRawMidenClient(client, midenRpcEndpoint);
  return rawClient.executeForSummaryAt(acc, txRequest, anchor);
}

/**
 * Serializes a `ChainAnchor` to base64 for the proposal wire payload.
 */
export function chainAnchorToBase64(anchor: ChainAnchor): string {
  return uint8ArrayToBase64(anchor.serialize());
}

/**
 * Deserializes a `ChainAnchor` from its base64 wire form. `ChainAnchor`
 * deserialization validates the header/chain consistency internally, so a
 * decoded anchor only needs its block commitment checked against the signed
 * transaction summary before it is safe to execute against.
 */
export function chainAnchorFromBase64(anchorBase64: string): ChainAnchor {
  return ChainAnchor.deserialize(base64ToUint8Array(anchorBase64));
}

/**
 * Reads the auth args back out of a transaction summary.
 *
 * Since miden-protocol 0.16-rc the summary binds seven user-defined elements
 * instead of a dedicated salt word. The guarded-multisig auth component zeroes
 * the leading three and passes the auth args as the trailing four, so the auth
 * args are the tail of `userParams()`.
 *
 * This is the auth-arg word, *not* the proposal salt. When the request declares
 * a fee conversion salt, miden-client uses it to commit the native conversion
 * info under `hash(CONVERSION_INFO || SALT)`. That commitment is not invertible
 * to the salt. Keep the salt alongside the proposal — `ProposalMetadata.saltHex`
 * — rather than trying to recover it from the summary.
 */
export function summaryAuthArg(summary: TransactionSummary): Word {
  return Word.newFromFelts(summary.userParams().slice(AUTH_ARG_USER_PARAM_OFFSET));
}
