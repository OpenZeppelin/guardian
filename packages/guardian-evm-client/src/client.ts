import { requestWalletAddress, signTypedData, type Eip1193Provider } from './auth.js';
import { normalizeEvmAddress } from './encoding.js';
import { GuardianEvmHttpError } from './errors.js';
import {
  fromServerAccountRegistration,
  accountProposalParams,
  evmAccountId,
  fromServerExecutable,
  fromServerProposal,
  toServerApproveRequest,
  toServerCancelRequest,
  toServerCreateProposalRequest,
  toServerRegisterAccountRequest,
  type AccountRegistration,
  type ApproveRequest,
  type ConfigureRequest,
  type ExecutableProposal,
  type Proposal,
  type ProposeRequest,
  type ServerExecutableProposal,
  type ServerListProposalsResponse,
  type ServerProposal,
  type ServerRegisterAccountResponse,
} from './proposals.js';
import { fromServerChallenge, type EvmSessionChallenge } from './typed-data.js';

export interface GuardianEvmClientOptions {
  guardianUrl: string;
  provider?: Eip1193Provider;
  signerAddress?: string;
}

export interface EvmSession {
  address: `0x${string}`;
  expiresAt: number;
}

interface ServerVerifyResponse {
  address: string;
  expires_at: number;
}

export class GuardianEvmClient {
  readonly guardianUrl: string;
  readonly provider?: Eip1193Provider;
  readonly signerAddress?: `0x${string}`;
  private cookieHeader?: string;

  constructor(options: GuardianEvmClientOptions) {
    this.guardianUrl = options.guardianUrl.replace(/\/$/, '');
    this.provider = options.provider;
    this.signerAddress = options.signerAddress
      ? normalizeEvmAddress(options.signerAddress)
      : undefined;
  }

  accountId(chainId: number, smartAccountAddress: string): string {
    return evmAccountId(chainId, smartAccountAddress);
  }

  async challenge(address?: string): Promise<EvmSessionChallenge> {
    const signer = await this.resolveSigner(address);
    const params = new URLSearchParams({ address: signer });
    const response = await this.fetch(`/evm/auth/challenge?${params}`, { method: 'GET' });
    return fromServerChallenge(await response.json());
  }

  async login(address?: string): Promise<EvmSession> {
    const challenge = await this.challenge(address);
    const provider = this.requireProvider();
    const signature = await signTypedData(provider, challenge.address, challenge.typedData);
    const response = await this.fetch('/evm/auth/verify', {
      method: 'POST',
      body: JSON.stringify({
        address: challenge.address,
        nonce: challenge.nonce,
        signature,
      }),
    });
    const server = (await response.json()) as ServerVerifyResponse;
    return {
      address: normalizeEvmAddress(server.address),
      expiresAt: server.expires_at,
    };
  }

  async logout(): Promise<void> {
    await this.fetch('/evm/auth/logout', { method: 'POST' });
  }

  async configure(request: ConfigureRequest): Promise<AccountRegistration> {
    const response = await this.fetch('/evm/accounts', {
      method: 'POST',
      body: JSON.stringify(toServerRegisterAccountRequest(request)),
    });
    return fromServerAccountRegistration((await response.json()) as ServerRegisterAccountResponse);
  }

  async createProposal(request: ProposeRequest): Promise<Proposal> {
    const response = await this.fetch('/evm/proposals', {
      method: 'POST',
      body: JSON.stringify(toServerCreateProposalRequest(request)),
    });
    return fromServerProposal((await response.json()) as ServerProposal);
  }

  async listProposals(accountId: string): Promise<Proposal[]> {
    const params = accountProposalParams(accountId);
    const response = await this.fetch(`/evm/proposals?${params}`, { method: 'GET' });
    const server = (await response.json()) as ServerListProposalsResponse;
    return server.proposals.map(fromServerProposal);
  }

  async getProposal(accountId: string, proposalId: string): Promise<Proposal> {
    const params = accountProposalParams(accountId);
    const response = await this.fetch(`/evm/proposals/${proposalId}?${params}`, { method: 'GET' });
    return fromServerProposal((await response.json()) as ServerProposal);
  }

  async approveProposal(
    accountId: string,
    proposalId: string,
    request: ApproveRequest
  ): Promise<Proposal> {
    const response = await this.fetch(`/evm/proposals/${proposalId}/approve`, {
      method: 'POST',
      body: JSON.stringify(toServerApproveRequest(accountId, request)),
    });
    return fromServerProposal((await response.json()) as ServerProposal);
  }

  async getExecutableProposal(
    accountId: string,
    proposalId: string
  ): Promise<ExecutableProposal> {
    const params = accountProposalParams(accountId);
    const response = await this.fetch(`/evm/proposals/${proposalId}/executable?${params}`, {
      method: 'GET',
    });
    return fromServerExecutable((await response.json()) as ServerExecutableProposal);
  }

  async cancelProposal(accountId: string, proposalId: string): Promise<void> {
    await this.fetch(`/evm/proposals/${proposalId}/cancel`, {
      method: 'POST',
      body: JSON.stringify(toServerCancelRequest(accountId)),
    });
  }

  private async resolveSigner(address?: string): Promise<`0x${string}`> {
    if (address) {
      return normalizeEvmAddress(address);
    }
    if (this.signerAddress) {
      return this.signerAddress;
    }
    return requestWalletAddress(this.requireProvider());
  }

  private requireProvider(): Eip1193Provider {
    if (!this.provider) {
      throw new Error('GuardianEvmClient requires a provider for wallet signing');
    }
    return this.provider;
  }

  private async fetch(path: string, init: RequestInit): Promise<Response> {
    const response = await fetch(`${this.guardianUrl}${path}`, {
      ...init,
      credentials: 'include',
      headers: {
        'Content-Type': 'application/json',
        ...(this.cookieHeader ? { Cookie: this.cookieHeader } : {}),
        ...init.headers,
      },
    });

    if (!response.ok) {
      const body = await response.text();
      throw new GuardianEvmHttpError(response.status, response.statusText, body);
    }

    const setCookie = response.headers.get('set-cookie');
    if (setCookie) {
      this.cookieHeader = setCookie.split(';')[0];
    }

    return response;
  }
}
