export interface GuardianOperatorChallengeView {
  domain: string;
  commitment: string;
  nonce: string;
  expires_at: string;
  signing_digest: string;
}

export interface GuardianOperatorChallengeResponse {
  success: boolean;
  challenge: GuardianOperatorChallengeView;
}

export interface GuardianVerifyOperatorResponse {
  success: boolean;
  operator_id: string;
  expires_at: string;
}

export interface GuardianLogoutOperatorResponse {
  success: boolean;
}

export type GuardianDashboardAccountStateStatus = 'available' | 'unavailable';

export interface GuardianDashboardAccountSummary {
  account_id: string;
  auth_scheme: string;
  authorized_signer_count: number;
  has_pending_candidate: boolean;
  current_commitment: string | null;
  state_status: GuardianDashboardAccountStateStatus;
  created_at: string;
  updated_at: string;
}

export interface GuardianDashboardAccountDetail extends GuardianDashboardAccountSummary {
  authorized_signer_ids: string[];
  state_created_at: string | null;
  state_updated_at: string | null;
}

export interface GuardianDashboardAccountsResponse {
  success: boolean;
  total_count: number;
  accounts: GuardianDashboardAccountSummary[];
}

export interface GuardianDashboardAccountResponse {
  success: boolean;
  account: GuardianDashboardAccountDetail;
}

export interface GuardianErrorResponse {
  success: boolean;
  error: string;
  retry_after_secs?: number;
}
