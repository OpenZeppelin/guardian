import type { AdviceMap, Word } from '@demox-labs/miden-sdk';
import type { SignatureScheme } from '../types.js';

export interface SignatureOptions {
  salt?: Word;
  signatureAdviceMap?: AdviceMap;
  signatureScheme?: SignatureScheme;
}

