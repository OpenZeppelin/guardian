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
   * Set it to the chain's own fee faucet — `Multisig.getFeeFaucetId()` reads it
   * from the block a request built now will be anchored at. `AuthGuardedMultisig`
   * calls `fee::pay_fee` before building the summary, so on a chain whose
   * `verification_base_fee` is non-zero an unset faucet produces an auth arg
   * with no advice preimage and the transaction aborts at proving with
   * `ERR_FEE_CONVERSION_INFO_MISSING`. Leaving it unset is only valid on a
   * zero-fee chain. See `transaction/feeAuth.ts`.
   *
   * The typed `create*Proposal` methods do not take this option; they always
   * commit the anchored block's faucet, which is what makes their proposals
   * rebuildable from `salt_hex` alone.
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
