/**
 * HTTP client for PSM server communication.
 */

import type {
  ConfigureRequest,
  ConfigureResponse,
  DeltaObject,
  DeltaProposalRequest,
  DeltaProposalResponse,
  ExecutionDelta,
  ProposalsResponse,
  PubkeyResponse,
  SignProposalRequest,
  Signer,
  StateObject,
} from './types.js';

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
 * HTTP client for PSM server.
 */
export class PsmHttpClient {
  private signer: Signer | null = null;
  private readonly baseUrl: string;

  constructor(baseUrl: string) {
    this.baseUrl = baseUrl;
  }

  /**
   * Set the signer for authenticated requests.
   */
  setSigner(signer: Signer): void {
    this.signer = signer;
  }

  /**
   * Get the PSM server's public key.
   */
  async getPubkey(): Promise<string> {
    const response = await this.fetch('/pubkey', { method: 'GET' });
    const data = (await response.json()) as PubkeyResponse;
    return data.pubkey;
  }

  /**
   * Configure/register an account on PSM.
   */
  async configure(request: ConfigureRequest): Promise<ConfigureResponse> {
    const response = await this.fetchAuthenticated('/configure', {
      method: 'POST',
      body: JSON.stringify(request),
    }, request.account_id);
    return (await response.json()) as ConfigureResponse;
  }

  /**
   * Get account state from PSM.
   */
  async getState(accountId: string): Promise<StateObject> {
    const params = new URLSearchParams({ account_id: accountId });
    const response = await this.fetchAuthenticated(`/state?${params}`, {
      method: 'GET',
    }, accountId);
    return (await response.json()) as StateObject;
  }

  /**
   * Get all delta proposals for an account.
   */
  async getDeltaProposals(accountId: string): Promise<DeltaObject[]> {
    const params = new URLSearchParams({ account_id: accountId });
    const response = await this.fetchAuthenticated(`/delta/proposal?${params}`, {
      method: 'GET',
    }, accountId);
    const data = (await response.json()) as ProposalsResponse;
    return data.proposals;
  }

  /**
   * Push a new delta proposal.
   */
  async pushDeltaProposal(request: DeltaProposalRequest): Promise<DeltaProposalResponse> {
    const response = await this.fetchAuthenticated('/delta/proposal', {
      method: 'POST',
      body: JSON.stringify(request),
    }, request.account_id);
    return (await response.json()) as DeltaProposalResponse;
  }

  /**
   * Sign a delta proposal.
   */
  async signDeltaProposal(request: SignProposalRequest): Promise<DeltaObject> {
    const response = await this.fetchAuthenticated('/delta/proposal', {
      method: 'PUT',
      body: JSON.stringify(request),
    }, request.account_id);
    return (await response.json()) as DeltaObject;
  }

  /**
   * Push a delta (execute a proposal).
   * The delta_payload must be in execution format: { data: "<base64>" }
   * (not proposal format: { tx_summary: { data }, signatures }).
   */
  async pushDelta(delta: ExecutionDelta): Promise<DeltaObject> {
    const response = await this.fetchAuthenticated('/delta', {
      method: 'POST',
      body: JSON.stringify(delta),
    }, delta.account_id);
    return (await response.json()) as DeltaObject;
  }

  /**
   * Get a specific delta by nonce.
   */
  async getDelta(accountId: string, nonce: number): Promise<DeltaObject> {
    const params = new URLSearchParams({
      account_id: accountId,
      nonce: nonce.toString(),
    });
    const response = await this.fetchAuthenticated(`/delta?${params}`, {
      method: 'GET',
    }, accountId);
    return (await response.json()) as DeltaObject;
  }

  /**
   * Get merged delta since a nonce.
   */
  async getDeltaSince(accountId: string, fromNonce: number): Promise<DeltaObject> {
    const params = new URLSearchParams({
      account_id: accountId,
      nonce: fromNonce.toString(),
    });
    const response = await this.fetchAuthenticated(`/delta/since?${params}`, {
      method: 'GET',
    }, accountId);
    return (await response.json()) as DeltaObject;
  }

  // ==========================================================================
  // Private helpers
  // ==========================================================================

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
