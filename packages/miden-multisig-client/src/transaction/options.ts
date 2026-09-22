import type { AdviceMap, Word } from '@miden-sdk/miden-sdk';
import type { SignatureScheme } from '../types.js';

export interface SignatureOptions {
  salt?: Word;
  signatureAdviceMap?: AdviceMap;
  signatureScheme?: SignatureScheme;
  midenRpcEndpoint?: string;
  /**
   * The block the transaction summary binds. Omitted, the store's sync height,
   * which is right for the party creating a proposal. A cosigner or executor
   * rebuilding a proposal pins it to the proposal's anchor block, or the rebuilt
   * summary can never match the signed one.
   */
  boundBlockNum?: number;
  /**
   * Blocks after the bound block at which the approvers' signatures stop
   * authorizing the transaction, so it must be included by then. Bound by the
   * summary, so a rebuild must pass the same value. At most 65535 blocks, the
   * furthest a transaction can expire after its reference block; omitted, the
   * approval never expires, which is the upstream default.
   */
  approvalExpirationDelta?: number;
}

export interface MidenClientSignatureOptions extends SignatureOptions {
  midenRpcEndpoint: string;
}

/**
 * Options for a request a multisig account executes. The account decides the
 * auth args the request has to carry, so every multisig builder needs it.
 */
export interface MultisigRequestOptions extends SignatureOptions {
  accountId: string;
}

export interface MidenClientMultisigRequestOptions extends MultisigRequestOptions {
  midenRpcEndpoint: string;
}
