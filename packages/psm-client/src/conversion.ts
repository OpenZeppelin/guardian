import type {
  CosignerSignature,
  ConfigureRequest,
  ConfigureResponse,
  DeltaObject,
  DeltaProposalRequest,
  DeltaStatus,
  ExecutionDelta,
  FalconSignature,
  ProposalMetadata,
  SignProposalRequest,
  StateObject,
} from './types.js';
import type {
  ServerCosignerSignature,
  ServerConfigureRequest,
  ServerConfigureResponse,
  ServerDeltaObject,
  ServerDeltaProposalRequest,
  ServerDeltaStatus,
  ServerExecutionDelta,
  ServerFalconSignature,
  ServerProposalMetadata,
  ServerSignProposalRequest,
  ServerStateObject,
} from './server-types.js';

type ServerProposalDeltaPayload = {
  tx_summary: { data: string };
  signatures?: Array<{ signer_id: string; signature: ServerFalconSignature }>;
  metadata?: ServerProposalMetadata;
};

type ServerExecutionDeltaPayload = {
  data: string;
  signatures?: Array<{ signer_id: string; signature: ServerFalconSignature }>;
  metadata?: ServerProposalMetadata;
};

type ServerDeltaPayload = ServerProposalDeltaPayload | ServerExecutionDeltaPayload;

function extractDeltaPayload(payload: ServerDeltaPayload): {
  txSummary: { data: string };
  signatures: Array<{ signer_id: string; signature: ServerFalconSignature }>;
  metadata?: ServerProposalMetadata;
} {
  const txSummary = 'tx_summary' in payload ? payload.tx_summary : { data: payload.data };
  const signatures = 'signatures' in payload && Array.isArray(payload.signatures) ? payload.signatures : [];
  const metadata = 'metadata' in payload ? payload.metadata : undefined;
  return { txSummary, signatures, metadata };
}

export function fromServerFalconSignature(server: ServerFalconSignature): FalconSignature {
  return server;
}

export function fromServerCosignerSignature(server: ServerCosignerSignature): CosignerSignature {
  return {
    signerId: server.signer_id,
    signature: fromServerFalconSignature(server.signature),
    timestamp: server.timestamp,
  };
}

export function fromServerDeltaStatus(server: ServerDeltaStatus): DeltaStatus {
  switch (server.status) {
    case 'pending':
      return {
        status: 'pending',
        timestamp: server.timestamp,
        proposerId: server.proposer_id,
        cosignerSigs: server.cosigner_sigs.map(fromServerCosignerSignature),
      };
    case 'candidate':
      return { status: 'candidate', timestamp: server.timestamp };
    case 'canonical':
      return { status: 'canonical', timestamp: server.timestamp };
    case 'discarded':
      return { status: 'discarded', timestamp: server.timestamp };
  }
}

export function fromServerProposalMetadata(server: ServerProposalMetadata): ProposalMetadata {
  return {
    proposalType: server.proposal_type,
    targetThreshold: server.target_threshold,
    signerCommitments: server.signer_commitments,
    salt: server.salt,
    description: server.description,
    newPsmPubkey: server.new_psm_pubkey,
    newPsmEndpoint: server.new_psm_endpoint,
    noteIds: server.note_ids,
    recipientId: server.recipient_id,
    faucetId: server.faucet_id,
    amount: server.amount,
  };
}

export function fromServerDeltaObject(server: ServerDeltaObject): DeltaObject {
  const { txSummary, signatures, metadata } = extractDeltaPayload(server.delta_payload as ServerDeltaPayload);

  return {
    accountId: server.account_id,
    nonce: server.nonce,
    prevCommitment: server.prev_commitment,
    newCommitment: server.new_commitment,
    deltaPayload: {
      txSummary,
      signatures: signatures.map((s) => ({
        signerId: s.signer_id,
        signature: fromServerFalconSignature(s.signature),
      })),
      metadata: metadata ? fromServerProposalMetadata(metadata) : undefined,
    },
    ackSig: server.ack_sig,
    status: fromServerDeltaStatus(server.status),
  };
}

export function fromServerStateObject(server: ServerStateObject): StateObject {
  return {
    accountId: server.account_id,
    commitment: server.commitment,
    stateJson: server.state_json,
    createdAt: server.created_at,
    updatedAt: server.updated_at,
  };
}

export function fromServerConfigureResponse(server: ServerConfigureResponse): ConfigureResponse {
  return {
    success: server.success,
    message: server.message,
    ackPubkey: server.ack_pubkey,
  };
}

// ============================================================================
// Request conversions (camelCase → server)
// ============================================================================

export function toServerFalconSignature(sig: FalconSignature): ServerFalconSignature {
  return sig;
}

export function toServerCosignerSignature(sig: CosignerSignature): ServerCosignerSignature {
  return {
    signer_id: sig.signerId,
    signature: toServerFalconSignature(sig.signature),
    timestamp: sig.timestamp,
  };
}

export function toServerDeltaStatus(status: DeltaStatus): ServerDeltaStatus {
  switch (status.status) {
    case 'pending':
      return {
        status: 'pending',
        timestamp: status.timestamp,
        proposer_id: status.proposerId,
        cosigner_sigs: status.cosignerSigs.map(toServerCosignerSignature),
      };
    case 'candidate':
      return { status: 'candidate', timestamp: status.timestamp };
    case 'canonical':
      return { status: 'canonical', timestamp: status.timestamp };
    case 'discarded':
      return { status: 'discarded', timestamp: status.timestamp };
  }
}

export function toServerProposalMetadata(meta: ProposalMetadata): ServerProposalMetadata {
  return {
    proposal_type: meta.proposalType,
    target_threshold: meta.targetThreshold,
    signer_commitments: meta.signerCommitments,
    salt: meta.salt,
    description: meta.description,
    new_psm_pubkey: meta.newPsmPubkey,
    new_psm_endpoint: meta.newPsmEndpoint,
    note_ids: meta.noteIds,
    recipient_id: meta.recipientId,
    faucet_id: meta.faucetId,
    amount: meta.amount,
  };
}

export function toServerConfigureRequest(req: ConfigureRequest): ServerConfigureRequest {
  return {
    account_id: req.accountId,
    auth: req.auth,
    initial_state: { data: req.initialState.data, account_id: req.initialState.accountId },
    storage_type: req.storageType,
  };
}

export function toServerDeltaProposalRequest(req: DeltaProposalRequest): ServerDeltaProposalRequest {
  return {
    account_id: req.accountId,
    nonce: req.nonce,
    delta_payload: {
      tx_summary: req.deltaPayload.txSummary,
      signatures: req.deltaPayload.signatures.map((s) => ({
        signer_id: s.signerId,
        signature: toServerFalconSignature(s.signature),
      })),
      metadata: req.deltaPayload.metadata ? toServerProposalMetadata(req.deltaPayload.metadata) : undefined,
    },
  };
}

export function toServerSignProposalRequest(req: SignProposalRequest): ServerSignProposalRequest {
  return {
    account_id: req.accountId,
    commitment: req.commitment,
    signature: toServerFalconSignature(req.signature),
  };
}

export function toServerExecutionDelta(delta: ExecutionDelta): ServerExecutionDelta {
  return {
    account_id: delta.accountId,
    nonce: delta.nonce,
    prev_commitment: delta.prevCommitment,
    new_commitment: delta.newCommitment,
    delta_payload: delta.deltaPayload,
    ack_sig: delta.ackSig,
    status: toServerDeltaStatus(delta.status),
  };
}
