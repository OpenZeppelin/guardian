export {
  GuardianOperatorContractError,
  GuardianOperatorHttpClient,
  GuardianOperatorHttpError,
  isDashboardErrorCode,
  parseErrorBody,
} from './http.js';

export type { PaginationOptions, ParsedErrorBody } from './http.js';

export {
  ACCOUNTS_PAUSE,
  DASHBOARD_READ,
  POLICIES_WRITE,
  REQUIRED_PERMISSIONS,
  requiredPermissionsFor,
} from './permissions.js';

export type {
  DashboardEndpointKey,
  OperatorPermission,
} from './permissions.js';

export type {
  DashboardAccountDetail,
  DashboardAccountResponse,
  DashboardAccountStateStatus,
  DashboardAccountSummary,
  DashboardDeltaEntry,
  DashboardDeltaStatus,
  DashboardErrorCode,
  DashboardGlobalDeltaEntry,
  DashboardGlobalDeltaStatusFilter,
  DashboardGlobalProposalEntry,
  DashboardInfoResponse,
  DashboardProposalEntry,
  GlobalDeltasOptions,
  GuardianOperatorHttpClientOptions,
  GuardianOperatorHttpErrorData,
  LogoutOperatorResponse,
  OperatorChallenge,
  OperatorChallengeResponse,
  PagedResult,
  VerifyOperatorRequest,
  VerifyOperatorResponse,
} from './types.js';
