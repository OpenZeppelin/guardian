import { Account, TransactionSummary } from '@demox-labs/miden-sdk';
import { base64ToUint8Array } from '../utils/encoding.js';

export function computeCommitmentFromTxSummary(txSummaryBase64: string): string {
  const bytes = base64ToUint8Array(txSummaryBase64);
  const summary = TransactionSummary.deserialize(bytes);
  const commitment = summary.toCommitment();
  return commitment.toHex();
}
export function accountIdToHex(account: Account): string {
  const accountId = account.id();
  const str = accountId.toString();
  if (str.startsWith('0x') || str.startsWith('0X')) {
    return str;
  }
  const prefix = accountId.prefix().asInt();
  const suffix = accountId.suffix().asInt();
  const prefixHex = prefix.toString(16).padStart(16, '0');
  const suffixHex = suffix.toString(16).padStart(16, '0');
  return `0x${prefixHex}${suffixHex.slice(0, 14)}`;
}

