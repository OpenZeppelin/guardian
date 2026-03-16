import type {
  ConfigureRequest,
  ConfigureResponse,
  DeltaObject,
  DeltaProposalRequest,
  DeltaProposalResponse,
  ExecutionDelta,
  PubkeyResponse,
  PushDeltaResponse,
  SignProposalRequest,
  SignatureScheme,
  Signer,
  StateObject,
} from './types.js';
import { RequestAuthPayload } from './auth-request.js';
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
  private lastTimestamp = 0;

  constructor(baseUrl: string) {
    this.baseUrl = baseUrl;
  }

  setSigner(signer: Signer): void {
    this.signer = signer;
  }

  async getPubkey(scheme?: SignatureScheme): Promise<PubkeyResponse> {
    const query = scheme ? `?scheme=${scheme}` : '';
    const response = await this.fetch(`/pubkey${query}`, { method: 'GET' });
    const data = (await response.json()) as ServerPubkeyResponse;
    return {
      commitment: data.commitment,
      pubkey: data.pubkey,
    };
  }

  async configure(request: ConfigureRequest): Promise<ConfigureResponse> {
    const serverRequest = toServerConfigureRequest(request);
    const response = await this.fetchAuthenticated('/configure', {
      method: 'POST',
      body: JSON.stringify(serverRequest),
    }, request.accountId, serverRequest);
    const server = (await response.json()) as ServerConfigureResponse;
    return fromServerConfigureResponse(server);
  }

  async getState(accountId: string): Promise<StateObject> {
    const requestQuery = { account_id: accountId };
    const params = new URLSearchParams(requestQuery);
    const response = await this.fetchAuthenticated(`/state?${params}`, {
      method: 'GET',
    }, accountId, requestQuery);
    const server = (await response.json()) as ServerStateObject;
    return fromServerStateObject(server);
  }

  async getDeltaProposals(accountId: string): Promise<DeltaObject[]> {
    const requestQuery = { account_id: accountId };
    const params = new URLSearchParams(requestQuery);
    const response = await this.fetchAuthenticated(`/delta/proposal?${params}`, {
      method: 'GET',
    }, accountId, requestQuery);
    const data = (await response.json()) as ServerProposalsResponse;
    return data.proposals.map(fromServerDeltaObject);
  }

  async getDeltaProposal(accountId: string, commitment: string): Promise<DeltaObject> {
    const requestQuery = { account_id: accountId, commitment };
    const params = new URLSearchParams(requestQuery);
    const response = await this.fetchAuthenticated(`/delta/proposal/single?${params}`, {
      method: 'GET',
    }, accountId, requestQuery);
    const data = (await response.json()) as ServerDeltaObject;
    return fromServerDeltaObject(data);
  }

  async pushDeltaProposal(request: DeltaProposalRequest): Promise<DeltaProposalResponse> {
    const serverRequest = toServerDeltaProposalRequest(request);
    const response = await this.fetchAuthenticated('/delta/proposal', {
      method: 'POST',
      body: JSON.stringify(serverRequest),
    }, request.accountId, serverRequest);
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
    }, request.accountId, serverRequest);
    const server = (await response.json()) as ServerDeltaObject;
    return fromServerDeltaObject(server);
  }

  async pushDelta(delta: ExecutionDelta): Promise<PushDeltaResponse> {
    const serverDelta = toServerExecutionDelta(delta);
    const response = await this.fetchAuthenticated('/delta', {
      method: 'POST',
      body: JSON.stringify(serverDelta),
    }, delta.accountId, serverDelta);
    const server = (await response.json()) as ServerPushDeltaResponse;
    return {
      accountId: server.account_id,
      nonce: server.nonce,
      newCommitment: server.new_commitment,
      ackSig: server.ack_sig,
      ackPubkey: server.ack_pubkey,
      ackScheme: server.ack_scheme,
    };
  }

  async getDelta(accountId: string, nonce: number): Promise<DeltaObject> {
    const requestPayload = {
      account_id: accountId,
      nonce,
    };
    const requestQuery = {
      account_id: accountId,
      nonce: nonce.toString(),
    };
    const params = new URLSearchParams(requestQuery);
    const response = await this.fetchAuthenticated(`/delta?${params}`, {
      method: 'GET',
    }, accountId, requestPayload);
    const server = (await response.json()) as ServerDeltaObject;
    return fromServerDeltaObject(server);
  }

  async getDeltaSince(accountId: string, fromNonce: number): Promise<DeltaObject> {
    const requestPayload = {
      account_id: accountId,
      nonce: fromNonce,
    };
    const requestQuery = {
      account_id: accountId,
      nonce: fromNonce.toString(),
    };
    const params = new URLSearchParams(requestQuery);
    const response = await this.fetchAuthenticated(`/delta/since?${params}`, {
      method: 'GET',
    }, accountId, requestPayload);
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
    accountId: string,
    requestPayload: unknown,
    retries = 2
  ): Promise<Response> {
    if (!this.signer) {
      throw new Error('No signer configured. Call setSigner() first.');
    }

    const now = Date.now();
    const timestamp = now > this.lastTimestamp ? now : this.lastTimestamp + 1;
    this.lastTimestamp = timestamp;
    const authPayload = RequestAuthPayload.fromRequest(requestPayload);
    const signature = this.signer.signRequest
      ? await this.signer.signRequest(accountId, timestamp, authPayload)
      : await this.signer.signAccountIdWithTimestamp(accountId, timestamp);

    try {
      return await this.fetch(path, {
        ...init,
        headers: {
          ...init.headers,
          'x-pubkey': this.signer.publicKey,
          'x-signature': signature,
          'x-timestamp': timestamp.toString(),
        },
      });
    } catch (err) {
      if (retries > 0 && err instanceof PsmHttpError && err.body.includes('Replay attack')) {
        await new Promise((resolve) => setTimeout(resolve, 50));
        return this.fetchAuthenticated(path, init, accountId, requestPayload, retries - 1);
      }
      throw err;
    }
  }
}
