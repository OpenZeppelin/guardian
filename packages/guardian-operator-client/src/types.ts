export type DashboardAccountStateStatus = 'available' | 'unavailable';

export interface GuardianOperatorHttpErrorData {
  success: false;
  error: string;
  retryAfterSecs?: number;
}

export interface GuardianOperatorHttpClientOptions {
  baseUrl: string;
  fetch?: typeof fetch;
  credentials?: RequestCredentials;
  headers?: HeadersInit;
}

export interface OperatorChallenge {
  domain: string;
  commitment: string;
  nonce: string;
  expiresAt: string;
  signingDigest: string;
}

export interface OperatorChallengeResponse {
  success: true;
  challenge: OperatorChallenge;
}

export interface VerifyOperatorRequest {
  commitment: string;
  signature: string;
}

export interface VerifyOperatorResponse {
  success: true;
  operatorId: string;
  expiresAt: string;
}

export interface LogoutOperatorResponse {
  success: true;
}

export interface DashboardAccountSummary {
  accountId: string;
  authScheme: string;
  authorizedSignerCount: number;
  hasPendingCandidate: boolean;
  currentCommitment: string | null;
  stateStatus: DashboardAccountStateStatus;
  createdAt: string;
  updatedAt: string;
}

export interface DashboardAccountDetail extends DashboardAccountSummary {
  authorizedSignerIds: string[];
  stateCreatedAt: string | null;
  stateUpdatedAt: string | null;
}

export interface DashboardAccountsResponse {
  success: true;
  totalCount: number;
  accounts: DashboardAccountSummary[];
}

export interface DashboardAccountResponse {
  success: true;
  account: DashboardAccountDetail;
}
