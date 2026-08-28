import type { AccountId, AdviceMap, Word } from '@miden-sdk/miden-sdk';
import type { SignatureScheme } from '../types.js';

export interface SignatureOptions {
  salt?: Word;
  /**
   * Faucet whose asset pays the transaction fee, at rate 1/1.
   *
   * When set, the request's auth arg becomes the conversion-info commitment
   * `hash(CONVERSION_INFO || SALT)` that `fee::load_conversion_info` requires,
   * and the preimage is added to the advice map.
   *
   * Leave it unset for a guarded-multisig custody account. That auth component
   * never calls `load_conversion_info`, so the commitment would not be read,
   * and a proposal carrying one cannot be rebuilt by the Rust SDK. Setting it
   * does not make a custody transaction pay fees — see
   * `transaction/feeAuth.ts` and `docs/MIDEN_COMPATIBILITY.md`.
   *
   * For a proposal this SDK will rebuild, only the fee faucet of the proposal's
   * anchored block resolves: recovery derives its one candidate from that header
   * and assumes rate 1/1. A commitment under any other faucet, or a non-native
   * rate, is unrecoverable. For every type but `switch_guardian` that proposal
   * then cannot be synced, imported, signed or executed — and because
   * `syncProposals()` verifies each proposal without skipping, one such proposal
   * fails the whole account's sync for as long as GUARDIAN serves it. Only
   * `exportProposal()` still works, since it reads GUARDIAN's copy without
   * rebuilding anything.
   * `switch_guardian` executes without the fee advice instead, rather than
   * handing the outgoing GUARDIAN a veto.
   */
  feeFaucetId?: AccountId | string;
  signatureAdviceMap?: AdviceMap;
  signatureScheme?: SignatureScheme;
  midenRpcEndpoint?: string;
}

export interface MidenClientSignatureOptions extends SignatureOptions {
  midenRpcEndpoint: string;
}
