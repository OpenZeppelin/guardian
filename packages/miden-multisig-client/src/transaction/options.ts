import type { AdviceMap, Word } from '@demox-labs/miden-sdk';

export interface SignatureOptions {
  salt?: Word;
  signatureAdviceMap?: AdviceMap;
}

