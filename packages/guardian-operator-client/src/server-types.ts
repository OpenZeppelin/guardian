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
  /**
   * Feature 001-account-pausing FR-005: RFC 3339 UTC timestamp of the
   * original pause; `null` when the account is active.
   */
  paused_at: string | null;
  /**
   * Feature 001-account-pausing FR-005: reason captured at first pause;
   * `null` when the account is active.
   */
  paused_reason: string | null;
}

export type GuardianAccountStatus = 'active' | 'paused';

export interface GuardianPauseAccountResponse {
  account_id: string;
  before_state: GuardianAccountStatus;
  after_state: GuardianAccountStatus;
  paused_at: string;
  paused_reason: string;
}

export interface GuardianUnpauseAccountResponse {
  account_id: string;
  before_state: GuardianAccountStatus;
  after_state: GuardianAccountStatus;
  reason: string | null;
}

/**
 * Feature 001-account-pausing FR-010. Stable error code returned by
 * mutating endpoints when the target account is paused. HTTP 409,
 * gRPC FAILED_PRECONDITION.
 */
export const GUARDIAN_ACCOUNT_PAUSED = 'GUARDIAN_ACCOUNT_PAUSED' as const;

export interface GuardianAccountPausedErrorDetails {
  paused_at: string;
  paused_reason: string | null;
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
  code?: string;
  retryable?: boolean;
  /** Populated only for `GUARDIAN_ACCOUNT_PAUSED`. */
  paused_at?: string;
  /** Populated only for `GUARDIAN_ACCOUNT_PAUSED`. */
  paused_reason?: string;
  /** Populated only for `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`. */
  missing_permissions?: string[];
}
