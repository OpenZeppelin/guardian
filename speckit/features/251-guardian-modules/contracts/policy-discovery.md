# Contract: Policy Discovery

**Source**: RFC 0002 sections 2.7 and 5.3. Client API, HTTP and gRPC, under the same authentication as other account reads. Only the account's cosigners may call either route.

## Routes

| HTTP | gRPC | Slice | Success |
|---|---|---|---|
| `GET /policies?account_id=…` | `GetPolicies` | M2a | 200 `PoliciesView` |
| `POST /delta/proposal/check` | `CheckDeltaProposal` | M2a | 200 `CheckResult`, or the errors proposal creation returns (including those of [client-errors.md](./client-errors.md)), or `rate_limit_exceeded` |

## Schemas

```text
PolicyEntry {
  module_id: string,
  hooks: string[],
  description: string,          // disclosure = summary or params
  params?: object,              // disclosure = params: the global params for scope = all, the account's own params for a default or assigned instance
  account_layer?: object        // disclosure = params, scope = all: this account's layer, when set
}
PoliciesView  { account_id: string, policies: PolicyEntry[] }   // evaluation order; instances with disclosure = none are absent

CheckDeltaProposal request: the proposal creation body (`DeltaProposalRequest`)
CheckResult   { allowed: true, complete: bool }   // complete = false whenever the catalog declares an enabled gate instance with dry_run = false, whether or not it applies to the account; never names it
```

## Rules

- The check runs creation's own checks (authentication, queue admission, the pending-proposal limit, delta verification), then the `before_proposal_created` instances, then the instances subscribed only to `before_transaction_approved`, each with its own hook, `dry_run = true` and the evaluation time as `accounting_time`; it stores nothing, takes no proposal slot and records no decision. The rate limit is admitted before the state is rebuilt.
- gRPC carries `params` and `account_layer` as JSON strings; HTTP as JSON objects.
- Rate limit per account and signer: `GUARDIAN_MODULE_CHECK_RATE_PER_MIN` (default 30); excess returns the existing `rate_limit_exceeded`.
- Every check is logged with the account and signer.
- The answer is advisory; `before_transaction_approved` stays the enforcement point.

## Clients

- `guardian-client` (Rust): `get_policies`, `check_delta_proposal`. `@openzeppelin/guardian-client` (TypeScript): `getPolicies`, `checkDeltaProposal`.
- Multisig SDKs: Rust `get_policies` and `check_transaction` (`check_transaction_with_options`), building the transaction exactly as `propose_transaction` does; TypeScript `getPolicies` and `checkTransaction`, with the same typed errors as proposal creation.

## Prototype status (#554)

Both routes, both transports, the Rust and TypeScript base clients and the Rust multisig SDK are built and tested. The TypeScript multisig SDK methods wait for a `@openzeppelin/guardian-client` release: that package resolves the client from npm, so calling unpublished methods would break its build.
