import type { AdviceMap, Word } from '@miden-sdk/miden-sdk';
import type { SignatureScheme } from '@openzeppelin/psm-client';

export interface SignatureOptions {
  salt?: Word;
  signatureAdviceMap?: AdviceMap;
  signatureScheme?: SignatureScheme;
}

