export { GuardianEvmClient, type EvmSession } from './client.js';
export {
  requestWalletAddress,
  signProposalHash,
  signTypedData,
  type Eip1193Provider,
} from './auth.js';
export { normalizeBytes32, normalizeEvmAddress, normalizeSignature, normalizeUint256String } from './encoding.js';
export { GuardianEvmHttpError } from './errors.js';
export {
  buildEvmSessionTypedData,
  type EvmSessionChallenge,
  type GuardianTypedData,
  type TypedDataField,
} from './typed-data.js';
export {
  evmAccountId,
  type AccountRegistration,
  type ApproveRequest,
  type ConfigureRequest,
  type EvmProposalSignature,
  type ExecutableProposal,
  type Proposal,
  type ProposeRequest,
} from './proposals.js';
