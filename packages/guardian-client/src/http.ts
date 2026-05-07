import type {
  ConfigureRequest,
  ConfigureResponse,
  DeltaObject,
  DeltaProposalRequest,
  DeltaProposalResponse,
  ExecutionDelta,
  LookupResponse,
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
  ServerLookupResponse,
  ServerProposalsResponse,
  ServerPubkeyResponse,
  ServerStateObject,
  ServerConfigureResponse,
  ServerPushDeltaResponse,
} from './server-types.js';
import {
  fromServerConfigureResponse,
  fromServerDeltaObject,
  fromServerLookupResponse,
  fromServerStateObject,
  toServerConfigureRequest,
  toServerDeltaProposalRequest,
  toServerExecutionDelta,
  toServerSignProposalRequest,
} from './conversion.js';

/**
 * Error thrown by the GUARDIAN HTTP client.
 */
export class GuardianHttpError extends Error {
  constructor(
    public readonly status: number,
    public readonly statusText: string,
    public readonly body: string
  ) {
    super(`GUARDIAN HTTP error ${status}: ${statusText} - ${body}`);
    this.name = 'GuardianHttpError';
  }
}

/**
 * Minimal HTTP client for GUARDIAN server.
 */
export class GuardianHttpClient {
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

  /**
   * Resolve a public-key commitment to the set of account IDs whose
   * authorization set contains it. Authentication is by proof-of-possession:
   * the configured signer MUST hold the private key behind `keyCommitmentHex`
   * and implement `signLookupMessage`. Returns an empty list when the
   * commitment is not authorized for any account.
   */
  async lookupAccountByKeyCommitment(keyCommitmentHex: string): Promise<LookupResponse> {
    const params = new URLSearchParams({ key_commitment: keyCommitmentHex });
    const response = await this.fetchLookupAuthenticated(
      `/state/lookup?${params}`,
      { method: 'GET' },
      keyCommitmentHex
    );
    return fromServerLookupResponse((await response.json()) as ServerLookupResponse);
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
      throw new GuardianHttpError(response.status, response.statusText, body);
    }

    return response;
  }

  /**
   * Authenticated fetch for the lookup endpoint. Cannot reuse
   * `fetchAuthenticated`, which builds an `AuthRequestPayload` bound to an
   * `accountId` (the value lookup is trying to discover). Digest construction
   * is delegated to the signer's `signLookupMessage`.
   */
  private async fetchLookupAuthenticated(
    path: string,
    init: RequestInit,
    keyCommitmentHex: string
  ): Promise<Response> {
    if (!this.signer) {
      throw new Error('No signer configured. Call setSigner() first.');
    }
    if (!this.signer.signLookupMessage) {
      throw new Error(
        'Signer does not implement signLookupMessage. Account recovery by key requires a ' +
          'signer that produces signatures over LookupAuthMessage::to_word; the canonical ' +
          'helper lives in @openzeppelin/miden-multisig-client.'
      );
    }

    const now = Date.now();
    const timestamp = now > this.lastTimestamp ? now : this.lastTimestamp + 1;
    this.lastTimestamp = timestamp;
    const signature = await this.signer.signLookupMessage(keyCommitmentHex, timestamp);

    return this.fetch(path, {
      ...init,
      headers: {
        ...init.headers,
        'x-pubkey': this.signer.publicKey,
        'x-signature': signature,
        'x-timestamp': timestamp.toString(),
      },
    });
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
      if (retries > 0 && err instanceof GuardianHttpError && err.body.includes('Replay attack')) {
        await new Promise((resolve) => setTimeout(resolve, 50));
        return this.fetchAuthenticated(path, init, accountId, requestPayload, retries - 1);
      }
      throw err;
    }
  }
}
