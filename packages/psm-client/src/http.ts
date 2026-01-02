import type {
  ConfigureRequest,
  ConfigureResponse,
  DeltaObject,
  DeltaProposalRequest,
  DeltaProposalResponse,
  ExecutionDelta,
  PushDeltaResponse,
  SignProposalRequest,
  Signer,
  StateObject,
} from './types.js';
import type {
  ServerDeltaObject,
  ServerDeltaProposalResponse,
  ServerProposalsResponse,
  ServerPubkeyResponse,
  ServerStateObject,
  ServerConfigureResponse,
  ServerPushDeltaResponse,
} from './server-types.js';
import {
  fromServerConfigureResponse,
  fromServerDeltaObject,
  fromServerStateObject,
  toServerConfigureRequest,
  toServerDeltaProposalRequest,
  toServerExecutionDelta,
  toServerSignProposalRequest,
} from './conversion.js';

/**
 * Error thrown by the PSM HTTP client.
 */
export class PsmHttpError extends Error {
  constructor(
    public readonly status: number,
    public readonly statusText: string,
    public readonly body: string
  ) {
    super(`PSM HTTP error ${status}: ${statusText} - ${body}`);
    this.name = 'PsmHttpError';
  }
}

/**
 * Minimal HTTP client for PSM server.
 */
export class PsmHttpClient {
  private signer: Signer | null = null;
  private readonly baseUrl: string;

  constructor(baseUrl: string) {
    this.baseUrl = baseUrl;
  }

  setSigner(signer: Signer): void {
    this.signer = signer;
  }

  async getPubkey(): Promise<string> {
    const response = await this.fetch('/pubkey', { method: 'GET' });
    const data = (await response.json()) as ServerPubkeyResponse;
    return data.pubkey;
  }

  async configure(request: ConfigureRequest): Promise<ConfigureResponse> {
    const serverRequest = toServerConfigureRequest(request);
    const response = await this.fetchAuthenticated('/configure', {
      method: 'POST',
      body: JSON.stringify(serverRequest),
    }, request.accountId);
    const server = (await response.json()) as ServerConfigureResponse;
    return fromServerConfigureResponse(server);
  }

  async getState(accountId: string): Promise<StateObject> {
    const params = new URLSearchParams({ account_id: accountId });
    const response = await this.fetchAuthenticated(`/state?${params}`, {
      method: 'GET',
    }, accountId);
    const server = (await response.json()) as ServerStateObject;
    return fromServerStateObject(server);
  }

  async getDeltaProposals(accountId: string): Promise<DeltaObject[]> {
    const params = new URLSearchParams({ account_id: accountId });
    const response = await this.fetchAuthenticated(`/delta/proposal?${params}`, {
      method: 'GET',
    }, accountId);
    const data = (await response.json()) as ServerProposalsResponse;
    return data.proposals.map(fromServerDeltaObject);
  }

  async pushDeltaProposal(request: DeltaProposalRequest): Promise<DeltaProposalResponse> {
    const serverRequest = toServerDeltaProposalRequest(request);
    const response = await this.fetchAuthenticated('/delta/proposal', {
      method: 'POST',
      body: JSON.stringify(serverRequest),
    }, request.accountId);
    const server = (await response.json()) as ServerDeltaProposalResponse;
    return {
      delta: fromServerDeltaObject(server.delta),
      commitment: server.commitment,
    };
  }

  async signDeltaProposal(request: SignProposalRequest): Promise<DeltaObject> {
    const serverRequest = toServerSignProposalRequest(request);
    const response = await this.fetchAuthenticated('/delta/proposal', {
      method: 'PUT',
      body: JSON.stringify(serverRequest),
    }, request.accountId);
    const server = (await response.json()) as ServerDeltaObject;
    return fromServerDeltaObject(server);
  }

  async pushDelta(delta: ExecutionDelta): Promise<PushDeltaResponse> {
    const serverDelta = toServerExecutionDelta(delta);
    const response = await this.fetchAuthenticated('/delta', {
      method: 'POST',
      body: JSON.stringify(serverDelta),
    }, delta.accountId);
    const server = (await response.json()) as ServerPushDeltaResponse;
    return {
      accountId: server.account_id,
      nonce: server.nonce,
      newCommitment: server.new_commitment,
      ackSig: server.ack_sig,
    };
  }

  async getDelta(accountId: string, nonce: number): Promise<DeltaObject> {
    const params = new URLSearchParams({
      account_id: accountId,
      nonce: nonce.toString(),
    });
    const response = await this.fetchAuthenticated(`/delta?${params}`, {
      method: 'GET',
    }, accountId);
    const server = (await response.json()) as ServerDeltaObject;
    return fromServerDeltaObject(server);
  }

  async getDeltaSince(accountId: string, fromNonce: number): Promise<DeltaObject> {
    const params = new URLSearchParams({
      account_id: accountId,
      nonce: fromNonce.toString(),
    });
    const response = await this.fetchAuthenticated(`/delta/since?${params}`, {
      method: 'GET',
    }, accountId);
    const server = (await response.json()) as ServerDeltaObject;
    return fromServerDeltaObject(server);
  }

  private async fetch(path: string, init: RequestInit): Promise<Response> {
    const url = `${this.baseUrl}${path}`;
    const response = await fetch(url, {
      ...init,
      headers: {
        'Content-Type': 'application/json',
        ...init.headers,
      },
    });

    if (!response.ok) {
      const body = await response.text();
      throw new PsmHttpError(response.status, response.statusText, body);
    }

    return response;
  }

  private async fetchAuthenticated(
    path: string,
    init: RequestInit,
    accountId: string
  ): Promise<Response> {
    if (!this.signer) {
      throw new Error('No signer configured. Call setSigner() first.');
    }

    const signature = this.signer.signAccountId(accountId);

    return this.fetch(path, {
      ...init,
      headers: {
        ...init.headers,
        'x-pubkey': this.signer.publicKey,
        'x-signature': signature,
      },
    });
  }
}
