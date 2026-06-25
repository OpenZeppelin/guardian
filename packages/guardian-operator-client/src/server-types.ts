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
  /** RFC 3339 UTC timestamp of the original pause; `null` when active. */
  paused_at: string | null;
  /** Reason captured at first pause; `null` when active. */
  paused_reason: string | null;
}

export interface GuardianDashboardAccountDetail extends GuardianDashboardAccountSummary {
  authorized_signer_ids: string[];
  state_created_at: string | null;
  state_updated_at: string | null;
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
 * Stable error code returned by mutating endpoints when the target
 * account is paused. HTTP 409, gRPC FAILED_PRECONDITION.
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

/** Structured machine-readable side-data on the wire (feature 009). */
export interface GuardianErrorMetaWire {
  /** Always present. */
  retryable: boolean;
  retry_after_secs?: number;
  /** Populated only for `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`. */
  missing_permissions?: string[];
  /** Populated only for `GUARDIAN_ACCOUNT_PAUSED`. */
  paused_at?: string;
  /** Populated only for `GUARDIAN_ACCOUNT_PAUSED`. */
  paused_reason?: string | null;
}

/**
 * Wire shape of a Guardian error body (feature `009-human-readable-errors`):
 * `{ code, message, meta }`. The legacy `success`/`error` fields are gone;
 * the diagnostic detail is logged server-side only.
 */
export interface GuardianErrorResponse {
  /** Stable machine-readable code; branch on this. */
  code?: string;
  /** Short, user-safe message — safe to display verbatim. */
  message: string;
  meta: GuardianErrorMetaWire;
}
