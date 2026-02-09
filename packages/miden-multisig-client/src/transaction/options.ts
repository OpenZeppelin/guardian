import type { AdviceMap, Word } from '@miden-sdk/miden-sdk';

export interface SignatureOptions {
  salt?: Word;
  signatureAdviceMap?: AdviceMap;
}

