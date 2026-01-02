import { describe, it, expect } from 'vitest';
import {
  fromServerCosignerSignature,
  fromServerConfigureResponse,
  fromServerDeltaObject,
  fromServerDeltaStatus,
  fromServerProposalMetadata,
  fromServerStateObject,
  toServerConfigureRequest,
  toServerCosignerSignature,
  toServerDeltaProposalRequest,
  toServerDeltaStatus,
  toServerExecutionDelta,
  toServerProposalMetadata,
  toServerSignProposalRequest,
} from './conversion.js';
import type {
  ServerCosignerSignature,
  ServerConfigureResponse,
  ServerDeltaObject,
  ServerDeltaStatus,
  ServerProposalMetadata,
  ServerStateObject,
} from './server-types.js';
import type {
  CosignerSignature,
  ConfigureRequest,
  DeltaProposalRequest,
  DeltaStatus,
  ExecutionDelta,
  ProposalMetadata,
  SignProposalRequest,
} from './types.js';

describe('conversion', () => {
  describe('fromServer conversions (server → camelCase)', () => {
    it('converts CosignerSignature', () => {
      const server: ServerCosignerSignature = {
        signer_id: '0xabc',
        signature: { scheme: 'falcon', signature: '0x123' },
        timestamp: '2024-01-01T00:00:00Z',
      };

      const result = fromServerCosignerSignature(server);

      expect(result).toEqual({
        signerId: '0xabc',
        signature: { scheme: 'falcon', signature: '0x123' },
        timestamp: '2024-01-01T00:00:00Z',
      });
    });

    it('converts pending DeltaStatus', () => {
      const server: ServerDeltaStatus = {
        status: 'pending',
        timestamp: '2024-01-01T00:00:00Z',
        proposer_id: '0xproposer',
        cosigner_sigs: [
          { signer_id: '0xsig1', signature: { scheme: 'falcon', signature: '0x1' }, timestamp: '2024-01-01T00:00:00Z' },
        ],
      };

      const result = fromServerDeltaStatus(server);

      expect(result).toEqual({
        status: 'pending',
        timestamp: '2024-01-01T00:00:00Z',
        proposerId: '0xproposer',
        cosignerSigs: [
          { signerId: '0xsig1', signature: { scheme: 'falcon', signature: '0x1' }, timestamp: '2024-01-01T00:00:00Z' },
        ],
      });
    });

    it('converts candidate/canonical/discarded DeltaStatus', () => {
      const statuses: ServerDeltaStatus[] = [
        { status: 'candidate', timestamp: '2024-01-01T00:00:00Z' },
        { status: 'canonical', timestamp: '2024-01-02T00:00:00Z' },
        { status: 'discarded', timestamp: '2024-01-03T00:00:00Z' },
      ];

      for (const server of statuses) {
        const result = fromServerDeltaStatus(server);
        expect(result.status).toBe(server.status);
        expect(result.timestamp).toBe(server.timestamp);
      }
    });

    it('converts ProposalMetadata with all fields', () => {
      const server: ServerProposalMetadata = {
        proposal_type: 'add_signer',
        target_threshold: 2,
        signer_commitments: ['0x1', '0x2'],
        salt: '0xsalt',
        description: 'test',
        new_psm_pubkey: '0xpubkey',
        new_psm_endpoint: 'http://psm',
        note_ids: ['0xnote1'],
        recipient_id: '0xrecipient',
        faucet_id: '0xfaucet',
        amount: '1000',
      };

      const result = fromServerProposalMetadata(server);

      expect(result).toEqual({
        proposalType: 'add_signer',
        targetThreshold: 2,
        signerCommitments: ['0x1', '0x2'],
        salt: '0xsalt',
        description: 'test',
        newPsmPubkey: '0xpubkey',
        newPsmEndpoint: 'http://psm',
        noteIds: ['0xnote1'],
        recipientId: '0xrecipient',
        faucetId: '0xfaucet',
        amount: '1000',
      });
    });

    it('converts ProposalMetadata with partial fields', () => {
      const server: ServerProposalMetadata = {
        proposal_type: 'p2id',
        description: 'send funds',
      };

      const result = fromServerProposalMetadata(server);

      expect(result.proposalType).toBe('p2id');
      expect(result.description).toBe('send funds');
      expect(result.targetThreshold).toBeUndefined();
    });

    it('converts DeltaObject', () => {
      const server: ServerDeltaObject = {
        account_id: '0xaccount',
        nonce: 1,
        prev_commitment: '0xprev',
        new_commitment: '0xnew',
        delta_payload: {
          tx_summary: { data: 'base64data' },
          signatures: [{ signer_id: '0xsig', signature: { scheme: 'falcon', signature: '0x123' } }],
          metadata: { proposal_type: 'add_signer', description: 'test' },
        },
        ack_sig: '0xack',
        status: { status: 'canonical', timestamp: '2024-01-01T00:00:00Z' },
      };

      const result = fromServerDeltaObject(server);

      expect(result.accountId).toBe('0xaccount');
      expect(result.nonce).toBe(1);
      expect(result.prevCommitment).toBe('0xprev');
      expect(result.newCommitment).toBe('0xnew');
      expect(result.deltaPayload.txSummary.data).toBe('base64data');
      expect(result.deltaPayload.signatures[0].signerId).toBe('0xsig');
      expect(result.deltaPayload.metadata?.proposalType).toBe('add_signer');
      expect(result.ackSig).toBe('0xack');
      expect(result.status.status).toBe('canonical');
    });

    it('converts DeltaObject without optional fields', () => {
      const server: ServerDeltaObject = {
        account_id: '0xaccount',
        nonce: 1,
        prev_commitment: '0xprev',
        delta_payload: {
          tx_summary: { data: 'base64data' },
          signatures: [],
        },
        status: { status: 'pending', timestamp: '2024-01-01T00:00:00Z', proposer_id: '0xp', cosigner_sigs: [] },
      };

      const result = fromServerDeltaObject(server);

      expect(result.newCommitment).toBeUndefined();
      expect(result.deltaPayload.metadata).toBeUndefined();
      expect(result.ackSig).toBeUndefined();
    });

    it('converts StateObject', () => {
      const server: ServerStateObject = {
        account_id: '0xaccount',
        commitment: '0xcommit',
        state_json: { data: 'statedata' },
        created_at: '2024-01-01T00:00:00Z',
        updated_at: '2024-01-02T00:00:00Z',
      };

      const result = fromServerStateObject(server);

      expect(result).toEqual({
        accountId: '0xaccount',
        commitment: '0xcommit',
        stateJson: { data: 'statedata' },
        createdAt: '2024-01-01T00:00:00Z',
        updatedAt: '2024-01-02T00:00:00Z',
      });
    });

    it('converts ConfigureResponse', () => {
      const server: ServerConfigureResponse = {
        success: true,
        message: 'configured',
        ack_pubkey: '0xpubkey',
      };

      const result = fromServerConfigureResponse(server);

      expect(result).toEqual({
        success: true,
        message: 'configured',
        ackPubkey: '0xpubkey',
      });
    });
  });

  describe('toServer conversions (camelCase → server)', () => {
    it('converts CosignerSignature', () => {
      const sig: CosignerSignature = {
        signerId: '0xabc',
        signature: { scheme: 'falcon', signature: '0x123' },
        timestamp: '2024-01-01T00:00:00Z',
      };

      const result = toServerCosignerSignature(sig);

      expect(result).toEqual({
        signer_id: '0xabc',
        signature: { scheme: 'falcon', signature: '0x123' },
        timestamp: '2024-01-01T00:00:00Z',
      });
    });

    it('converts pending DeltaStatus', () => {
      const status: DeltaStatus = {
        status: 'pending',
        timestamp: '2024-01-01T00:00:00Z',
        proposerId: '0xproposer',
        cosignerSigs: [
          { signerId: '0xsig1', signature: { scheme: 'falcon', signature: '0x1' }, timestamp: '2024-01-01T00:00:00Z' },
        ],
      };

      const result = toServerDeltaStatus(status);

      expect(result).toEqual({
        status: 'pending',
        timestamp: '2024-01-01T00:00:00Z',
        proposer_id: '0xproposer',
        cosigner_sigs: [
          { signer_id: '0xsig1', signature: { scheme: 'falcon', signature: '0x1' }, timestamp: '2024-01-01T00:00:00Z' },
        ],
      });
    });

    it('converts ProposalMetadata', () => {
      const meta: ProposalMetadata = {
        proposalType: 'add_signer',
        targetThreshold: 2,
        signerCommitments: ['0x1', '0x2'],
        salt: '0xsalt',
        description: 'test',
      };

      const result = toServerProposalMetadata(meta);

      expect(result).toEqual({
        proposal_type: 'add_signer',
        target_threshold: 2,
        signer_commitments: ['0x1', '0x2'],
        salt: '0xsalt',
        description: 'test',
        new_psm_pubkey: undefined,
        new_psm_endpoint: undefined,
        note_ids: undefined,
        recipient_id: undefined,
        faucet_id: undefined,
        amount: undefined,
      });
    });

    it('converts ConfigureRequest', () => {
      const req: ConfigureRequest = {
        accountId: '0xaccount',
        auth: { MidenFalconRpo: { cosigner_commitments: ['0x1', '0x2'] } },
        initialState: { data: 'statedata', accountId: '0xaccount' },
        storageType: 'Filesystem',
      };

      const result = toServerConfigureRequest(req);

      expect(result).toEqual({
        account_id: '0xaccount',
        auth: { MidenFalconRpo: { cosigner_commitments: ['0x1', '0x2'] } },
        initial_state: { data: 'statedata', account_id: '0xaccount' },
        storage_type: 'Filesystem',
      });
    });

    it('converts DeltaProposalRequest', () => {
      const req: DeltaProposalRequest = {
        accountId: '0xaccount',
        nonce: 1,
        deltaPayload: {
          txSummary: { data: 'base64data' },
          signatures: [{ signerId: '0xsig', signature: { scheme: 'falcon', signature: '0x123' } }],
          metadata: { proposalType: 'add_signer', description: 'test' },
        },
      };

      const result = toServerDeltaProposalRequest(req);

      expect(result.account_id).toBe('0xaccount');
      expect(result.nonce).toBe(1);
      expect(result.delta_payload.tx_summary.data).toBe('base64data');
      expect(result.delta_payload.signatures[0].signer_id).toBe('0xsig');
      expect(result.delta_payload.metadata?.proposal_type).toBe('add_signer');
    });

    it('converts SignProposalRequest', () => {
      const req: SignProposalRequest = {
        accountId: '0xaccount',
        commitment: '0xcommit',
        signature: { scheme: 'falcon', signature: '0x123' },
      };

      const result = toServerSignProposalRequest(req);

      expect(result).toEqual({
        account_id: '0xaccount',
        commitment: '0xcommit',
        signature: { scheme: 'falcon', signature: '0x123' },
      });
    });

    it('converts ExecutionDelta', () => {
      const delta: ExecutionDelta = {
        accountId: '0xaccount',
        nonce: 1,
        prevCommitment: '0xprev',
        newCommitment: '0xnew',
        deltaPayload: { data: 'base64data' },
        ackSig: '0xack',
        status: { status: 'canonical', timestamp: '2024-01-01T00:00:00Z' },
      };

      const result = toServerExecutionDelta(delta);

      expect(result.account_id).toBe('0xaccount');
      expect(result.nonce).toBe(1);
      expect(result.prev_commitment).toBe('0xprev');
      expect(result.new_commitment).toBe('0xnew');
      expect(result.delta_payload.data).toBe('base64data');
      expect(result.ack_sig).toBe('0xack');
      expect(result.status.status).toBe('canonical');
    });
  });

  describe('roundtrip conversions', () => {
    it('DeltaStatus survives roundtrip', () => {
      const original: DeltaStatus = {
        status: 'pending',
        timestamp: '2024-01-01T00:00:00Z',
        proposerId: '0xproposer',
        cosignerSigs: [
          { signerId: '0xsig', signature: { scheme: 'falcon', signature: '0x123' }, timestamp: '2024-01-01T00:00:00Z' },
        ],
      };

      const server = toServerDeltaStatus(original);
      const result = fromServerDeltaStatus(server);

      expect(result).toEqual(original);
    });

    it('ProposalMetadata survives roundtrip', () => {
      const original: ProposalMetadata = {
        proposalType: 'p2id',
        description: 'send funds',
        recipientId: '0xrecipient',
        faucetId: '0xfaucet',
        amount: '1000',
        salt: '0xsalt',
      };

      const server = toServerProposalMetadata(original);
      const result = fromServerProposalMetadata(server);

      expect(result.proposalType).toBe(original.proposalType);
      expect(result.description).toBe(original.description);
      expect(result.recipientId).toBe(original.recipientId);
      expect(result.faucetId).toBe(original.faucetId);
      expect(result.amount).toBe(original.amount);
      expect(result.salt).toBe(original.salt);
    });
  });
});
