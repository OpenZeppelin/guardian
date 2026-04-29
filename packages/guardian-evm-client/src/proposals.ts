import {
  normalizeBytes32,
  normalizeEvmAddress,
  normalizeSignature,
  normalizeUint256String,
} from './encoding.js';

export interface ConfigureRequest {
  chainId: number;
  smartAccountAddress: `0x${string}` | string;
  multisigValidatorAddress: `0x${string}` | string;
}

export interface AccountRegistration {
  accountId: string;
  chainId: number;
  accountAddress: `0x${string}`;
  multisigValidatorAddress: `0x${string}`;
  signers: `0x${string}`[];
  threshold: number;
}

export interface EvmProposalSignature {
  signer: `0x${string}`;
  signature: `0x${string}`;
  signedAt: number;
}

export interface ProposeRequest {
  accountId?: string;
  chainId: number;
  smartAccountAddress: `0x${string}` | string;
  userOpHash: `0x${string}` | string;
  payload: string;
  nonce: string;
  signature: `0x${string}` | string;
  ttlSeconds: number;
}

export interface Proposal {
  proposalId: string;
  accountId: string;
  chainId: number;
  smartAccountAddress: `0x${string}`;
  validatorAddress: `0x${string}`;
  userOpHash: `0x${string}`;
  payload: string;
  nonce: string;
  nonceKey: string;
  proposer: `0x${string}`;
  signerSnapshot: `0x${string}`[];
  threshold: number;
  signatures: EvmProposalSignature[];
  createdAt: number;
  expiresAt: number;
}

export interface ApproveRequest {
  signature: `0x${string}` | string;
}

export interface ExecutableProposal {
  hash: `0x${string}`;
  payload: string;
  signatures: `0x${string}`[];
  signers: `0x${string}`[];
}

export interface ServerRegisterAccountRequest {
  chain_id: number;
  account_address: string;
  multisig_validator_address: string;
}

export interface ServerRegisterAccountResponse {
  account_id: string;
  chain_id: number;
  account_address: string;
  multisig_validator_address: string;
  signers: string[];
  threshold: number;
}

export interface ServerCreateProposalRequest {
  account_id: string;
  user_op_hash: string;
  payload: string;
  nonce: string;
  ttl_seconds: number;
  signature: string;
}

export interface ServerProposal {
  proposal_id: string;
  account_id: string;
  chain_id: number;
  smart_account_address: string;
  validator_address: string;
  user_op_hash: string;
  payload: string;
  nonce: string;
  nonce_key: string;
  proposer: string;
  signer_snapshot: string[];
  threshold: number;
  signatures: ServerProposalSignature[];
  created_at: number;
  expires_at: number;
}

export interface ServerProposalSignature {
  signer: string;
  signature: string;
  signed_at: number;
}

export interface ServerListProposalsResponse {
  proposals: ServerProposal[];
}

export interface ServerApproveProposalRequest {
  account_id: string;
  signature: string;
}

export interface ServerCancelProposalRequest {
  account_id: string;
}

export interface ServerExecutableProposal {
  hash: string;
  payload: string;
  signatures: string[];
  signers: string[];
}

export function evmAccountId(chainId: number, smartAccountAddress: string): string {
  assertSafeChainId(chainId);
  return `evm:${chainId}:${normalizeEvmAddress(smartAccountAddress)}`;
}

export function toServerRegisterAccountRequest(
  request: ConfigureRequest
): ServerRegisterAccountRequest {
  assertSafeChainId(request.chainId);
  return {
    chain_id: request.chainId,
    account_address: normalizeEvmAddress(request.smartAccountAddress),
    multisig_validator_address: normalizeEvmAddress(request.multisigValidatorAddress),
  };
}

export function fromServerAccountRegistration(
  server: ServerRegisterAccountResponse
): AccountRegistration {
  return {
    accountId: stringField(server.account_id, 'account_id'),
    chainId: numberField(server.chain_id, 'chain_id'),
    accountAddress: normalizeEvmAddress(server.account_address),
    multisigValidatorAddress: normalizeEvmAddress(server.multisig_validator_address),
    signers: arrayField(server.signers, 'signers').map(normalizeEvmAddress),
    threshold: numberField(server.threshold, 'threshold'),
  };
}

export function toServerCreateProposalRequest(
  request: ProposeRequest
): ServerCreateProposalRequest {
  assertSafeChainId(request.chainId);
  assertPositiveSeconds(request.ttlSeconds, 'ttlSeconds');
  const accountId =
    request.accountId ?? evmAccountId(request.chainId, request.smartAccountAddress);
  return {
    account_id: accountId,
    user_op_hash: normalizeBytes32(request.userOpHash, 'userOpHash'),
    payload: request.payload,
    nonce: normalizeUint256String(request.nonce),
    ttl_seconds: request.ttlSeconds,
    signature: normalizeSignature(request.signature),
  };
}

export function toServerApproveRequest(
  accountId: string,
  request: ApproveRequest
): ServerApproveProposalRequest {
  return {
    account_id: accountId,
    signature: normalizeSignature(request.signature),
  };
}

export function toServerCancelRequest(accountId: string): ServerCancelProposalRequest {
  return { account_id: accountId };
}

export function fromServerProposal(server: ServerProposal): Proposal {
  return {
    proposalId: normalizeBytes32(server.proposal_id, 'proposal_id'),
    accountId: stringField(server.account_id, 'account_id'),
    chainId: numberField(server.chain_id, 'chain_id'),
    smartAccountAddress: normalizeEvmAddress(server.smart_account_address),
    validatorAddress: normalizeEvmAddress(server.validator_address),
    userOpHash: normalizeBytes32(server.user_op_hash, 'user_op_hash'),
    payload: stringField(server.payload, 'payload'),
    nonce: stringField(server.nonce, 'nonce'),
    nonceKey: stringField(server.nonce_key, 'nonce_key'),
    proposer: normalizeEvmAddress(server.proposer),
    signerSnapshot: arrayField(server.signer_snapshot, 'signer_snapshot').map(
      normalizeEvmAddress
    ),
    threshold: numberField(server.threshold, 'threshold'),
    signatures: arrayField(server.signatures, 'signatures').map(fromServerSignature),
    createdAt: numberField(server.created_at, 'created_at'),
    expiresAt: numberField(server.expires_at, 'expires_at'),
  };
}

export function fromServerExecutable(server: ServerExecutableProposal): ExecutableProposal {
  return {
    hash: normalizeBytes32(server.hash, 'hash'),
    payload: stringField(server.payload, 'payload'),
    signatures: arrayField(server.signatures, 'signatures').map(normalizeSignature),
    signers: arrayField(server.signers, 'signers').map(normalizeEvmAddress),
  };
}

export function accountProposalParams(accountId: string): URLSearchParams {
  return new URLSearchParams({ account_id: accountId });
}

function fromServerSignature(server: ServerProposalSignature): EvmProposalSignature {
  return {
    signer: normalizeEvmAddress(server.signer),
    signature: normalizeSignature(server.signature),
    signedAt: numberField(server.signed_at, 'signed_at'),
  };
}

function assertSafeChainId(chainId: number): void {
  if (!Number.isSafeInteger(chainId) || chainId <= 0) {
    throw new Error('chainId must be a positive safe integer');
  }
}

function assertPositiveSeconds(value: number, field: string): void {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${field} must be a positive safe integer`);
  }
}

function stringField(value: unknown, field: string): string {
  if (typeof value !== 'string') {
    throw new Error(`${field} must be a string`);
  }
  return value;
}

function numberField(value: unknown, field: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value)) {
    throw new Error(`${field} must be a safe integer`);
  }
  return value;
}

function arrayField<T>(value: T[] | undefined, field: string): T[] {
  if (!Array.isArray(value)) {
    throw new Error(`${field} must be an array`);
  }
  return value;
}
