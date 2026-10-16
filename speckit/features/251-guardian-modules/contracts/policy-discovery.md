# Contract: Policy Discovery

**Source**: RFC 0002 sections 2.7 and 5.3. Client API, HTTP and gRPC, under the same authentication as other account reads. Only the account's cosigners may call either route.

## Routes

| HTTP | gRPC | Slice | Success |
|---|---|---|---|
| `GET /accounts/{account_id}/policies` | `GetPolicies` | M2a | 200 `PoliciesView` |
| `POST /proposals/check` | `CheckProposal` | M2a | 200 `CheckResult`, or the denial and unavailability errors of [client-errors.md](./client-errors.md) |

## Schemas

```text
PolicyEntry {
  module_id: string,
  hooks: string[],
  description: string,          // disclosure = summary or params
  params?: object,              // disclosure = params: the effective global params
  account_layer?: object        // disclosure = params, scope = all: this account's layer, when set
}
PoliciesView  { account_id: string, policies: PolicyEntry[] }   // evaluation order; instances with disclosure = none are absent

CheckProposal request: the proposal creation body without signatures
CheckResult   { allowed: true, complete: bool }   // complete = false when an instance with dry_run = false was skipped; never names it
```

## Rules

- The check runs the `before_proposal_created` instances exactly as creation would, with the evaluation time as `accounting_time`, stores nothing, takes no proposal slot and records no decision.
- Rate limit per account and signer: `GUARDIAN_MODULE_CHECK_RATE_PER_MIN` (default 30); excess returns the existing `rate_limit_exceeded`.
- Every check is logged with the account and signer.
- The answer is advisory; `before_transaction_approved` stays the enforcement point.

## Clients

`getPolicies` and `checkProposal` in both multisig SDKs (Rust and TypeScript), with the same typed errors as proposal creation.

## Prototype status (#554)

Not built. The catalog fields these routes read (`disclosure`, `description`, `dry_run`) are parsed, validated and shown in the operator view.
